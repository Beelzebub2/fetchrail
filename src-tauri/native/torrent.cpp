#ifdef _WIN32
#define NOMINMAX
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <windows.h>
#endif
#include <libtorrent/session.hpp>
#include <libtorrent/session_params.hpp>
#include <libtorrent/mmap_disk_io.hpp>
#include <libtorrent/ip_filter.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/peer_info.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/read_resume_data.hpp>
#include <libtorrent/write_resume_data.hpp>
#include <libtorrent/load_torrent.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/hex.hpp>
#include <libtorrent/version.hpp>
#include "json.hpp"
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <filesystem>
#include <map>
#include <set>
#include <algorithm>
#include <cctype>
#include <optional>
#include <chrono>
#include <deque>
#include <thread>

namespace lt = libtorrent;
namespace fs = std::filesystem;
using json = nlohmann::json;

static std::string path_key(fs::path path, bool canonical = true) {
    path = (canonical ? fs::weakly_canonical(path) : path).lexically_normal();
#ifdef _WIN32
    auto wide = path.wstring();
    std::wstring lower(wide.size(), L'\0');
    if (!LCMapStringEx(LOCALE_NAME_INVARIANT, LCMAP_LOWERCASE, wide.data(), int(wide.size()), lower.data(), int(lower.size()), nullptr, nullptr, 0))
        throw std::runtime_error("Could not normalize the destination path");
    return fs::path(lower).u8string();
#else
    return path.u8string();
#endif
}

static std::vector<char> read_file(std::string const& path) {
    std::ifstream f(fs::u8path(path), std::ios::binary);
    if (!f) throw std::runtime_error("Cannot read torrent metadata or resume file");
    f.seekg(0, std::ios::end);
    auto size = f.tellg();
    if (size < 0 || size > 32 * 1024 * 1024) throw std::runtime_error("Torrent metadata exceeds 32 MiB");
    f.seekg(0);
    std::vector<char> bytes(static_cast<size_t>(size));
    if (!f.read(bytes.data(), size)) throw std::runtime_error("Incomplete torrent metadata");
    return bytes;
}

static void write_file(std::string const& path, std::vector<char> const& bytes) {
    auto dest = fs::u8path(path), temp = fs::u8path(path + ".tmp");
    fs::create_directories(dest.parent_path());
    std::ofstream f(temp, std::ios::binary | std::ios::trunc);
    f.write(bytes.data(), bytes.size()); f.flush();
    if (!f) throw std::runtime_error("Cannot save torrent state");
    f.close();
#ifdef _WIN32
    if (!MoveFileExW(temp.c_str(), dest.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
        throw std::runtime_error("Cannot replace torrent state");
#else
    fs::rename(temp, dest);
#endif
}

static json hashes(lt::info_hash_t const& h) {
    json out = json::array();
    if (h.has_v1()) out.push_back(lt::aux::to_hex(h.v1));
    if (h.has_v2()) out.push_back(lt::aux::to_hex(h.v2));
    return out;
}

static json metadata(lt::torrent_info const& ti) {
    json files = json::array();
    std::set<std::string> seen;
    for (auto i : ti.layout().file_range()) {
        if (ti.layout().pad_file_at(i)) continue;
        if (ti.layout().file_flags(i) & lt::file_storage::flag_symlink)
            throw std::runtime_error("Torrents containing symlinks are not supported");
        auto path = ti.layout().file_path(i);
        auto parsed = fs::u8path(path);
        if (parsed.is_absolute() || parsed.has_root_name()) throw std::runtime_error("Unsafe torrent file path");
        for (auto const& part : parsed) {
            auto name = part.u8string();
            if (name == ".." || name == "." || name.find(':') != std::string::npos)
                throw std::runtime_error("Unsafe torrent file path");
        }
        auto key = path_key(parsed, false);
        if (!seen.insert(key).second) throw std::runtime_error("Colliding torrent file names");
        files.push_back({{"index", int(i)}, {"path", path}, {"size", ti.layout().file_size(i)}, {"priority", 4}, {"downloaded", 0}});
    }
    if (files.empty()) throw std::runtime_error("Torrent has no payload files");
    return {{"name", ti.name()}, {"hashes", hashes(ti.info_hashes())}, {"private", ti.priv()},
        {"totalBytes", ti.total_size()}, {"files", files}, {"fileCount", ti.num_files()}, {"pieceLength", ti.piece_length()}, {"pieces", ti.num_pieces()}};
}

static lt::settings_pack network_settings(json const& c) {
    lt::settings_pack p;
    p.set_bool(lt::settings_pack::enable_dht, c.value("dht", true));
    p.set_bool(lt::settings_pack::enable_lsd, c.value("lsd", true));
    p.set_bool(lt::settings_pack::enable_upnp, c.value("upnp", true));
    p.set_bool(lt::settings_pack::enable_natpmp, c.value("upnp", true));
    p.set_int(lt::settings_pack::connections_limit, c.value("connections", 500));
    p.set_str(lt::settings_pack::outgoing_interfaces, c.value("outgoingInterfaces", ""));
    p.set_str(lt::settings_pack::listen_interfaces, c.value("listenInterfaces", "0.0.0.0:0,[::]:0"));
    p.set_int(lt::settings_pack::out_enc_policy, c.value("encryption", 1));
    p.set_int(lt::settings_pack::in_enc_policy, c.value("encryption", 1));
    auto protocol = c.value("protocol", 0);
    p.set_bool(lt::settings_pack::enable_incoming_tcp, protocol != 2);
    p.set_bool(lt::settings_pack::enable_outgoing_tcp, protocol != 2);
    p.set_bool(lt::settings_pack::enable_incoming_utp, protocol != 1);
    p.set_bool(lt::settings_pack::enable_outgoing_utp, protocol != 1);
    p.set_int(lt::settings_pack::proxy_type, c.value("proxyType", 0));
    p.set_str(lt::settings_pack::proxy_hostname, c.value("proxyHost", ""));
    p.set_int(lt::settings_pack::proxy_port, c.value("proxyPort", 0));
    p.set_str(lt::settings_pack::proxy_username, c.value("proxyUsername", ""));
    p.set_str(lt::settings_pack::proxy_password, c.value("proxyPassword", ""));
    p.set_bool(lt::settings_pack::proxy_peer_connections, true);
    p.set_bool(lt::settings_pack::proxy_tracker_connections, true);
    p.set_bool(lt::settings_pack::proxy_hostnames, true);
    return p;
}

struct Job {
    lt::torrent_handle handle;
    bool importing = false;
    std::string resume_path;
    std::string metadata_path;
    std::string error;
    bool moving = false;
    std::optional<lt::torrent_status> cached;
    std::chrono::steady_clock::time_point cached_at = std::chrono::steady_clock::now();
    bool flushed = false;
    bool flushing = false;
    int pending_saves = 0;
    bool removing = false;
    bool removed = false;
    std::set<std::string> paths;
    std::set<std::string> moving_paths;
    std::deque<json> activity;
};

struct Engine {
    std::unique_ptr<lt::session> session;
    std::map<std::string, Job> jobs;
    std::map<lt::torrent_handle, std::string> owners;
    std::vector<json> events;
    std::string root;
    std::string listen_request;
    int listen_retries = 0;
    std::map<std::string, std::string> path_owners;
    std::deque<json> session_activity;
    bool available(std::string const& key, std::string const& id = "") const {
        auto owned = [&](std::string const& path) { auto it = path_owners.find(path); return it != path_owners.end() && it->second != id; };
        if (owned(key)) return false;
        auto path = fs::u8path(key);
        for (auto parent = path.parent_path(); parent != path; path = parent, parent = parent.parent_path())
            if (owned(parent.u8string())) return false;
        auto prefix = key + char(fs::path::preferred_separator);
        for (auto it = path_owners.lower_bound(prefix); it != path_owners.end() && it->first.compare(0, prefix.size(), prefix) == 0; ++it)
            if (it->second != id) return false;
        return true;
    }
    std::set<std::string> claim(std::string const& id, std::string const& directory, json const& info) {
        std::set<std::string> paths;
        for (auto const& file : info["files"]) {
            auto key = path_key(fs::u8path(directory) / fs::u8path(file["path"].get<std::string>()));
            if (!available(key, id)) throw std::runtime_error("Another transfer owns this payload path. Choose another folder.");
            paths.insert(std::move(key));
        }
        for (auto const& path : paths) path_owners[path] = id;
        return paths;
    }
    void release(std::set<std::string> const& paths) { for (auto const& path : paths) path_owners.erase(path); }
    lt::session& get_session(json const& config = json::object()) {
        if (!session) {
            listen_request = config.value("listenInterfaces", "0.0.0.0:0,[::]:0");
            auto p = network_settings(config);
            p.set_str(lt::settings_pack::user_agent, "Fetchrail/0.5.2 libtorrent/" LIBTORRENT_VERSION);
            p.set_int(lt::settings_pack::alert_mask, lt::alert_category::error | lt::alert_category::storage | lt::alert_category::status | lt::alert_category::connect | lt::alert_category::tracker);
            p.set_int(lt::settings_pack::alert_queue_size, 4096);
            p.set_int(lt::settings_pack::hashing_threads, std::clamp(int(std::thread::hardware_concurrency() / 4), 1, 4));
            // Windows defaults to write-through, which serializes every validated piece.
            // Let the OS coalesce writes, then flush explicitly before completion/resume.
            p.set_int(lt::settings_pack::disk_io_write_mode, lt::settings_pack::enable_os_cache);
            p.set_int(lt::settings_pack::disk_io_read_mode, lt::settings_pack::enable_os_cache);
            lt::session_params params(p);
#if defined(_WIN64)
            // The 2.1 default is the experimental pread backend. Use the mature
            // Windows mapping backend; never mix mapped reads with normal writes.
            params.disk_io_constructor = lt::mmap_disk_io_constructor;
#endif
            session = std::make_unique<lt::session>(std::move(params));
            // Include private and loopback peers in the shared app bandwidth cap.
            lt::ip_filter classes;
            auto global = 1u << static_cast<std::uint32_t>(lt::session::global_peer_class_id);
            classes.add_rule(lt::address_v4::any(), lt::address_v4::broadcast(), global);
            classes.add_rule(lt::address_v6::any(), lt::make_address("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), global);
            session->set_peer_class_filter(classes);
        }
        return *session;
    }
    Job& job(std::string const& id) {
        auto i = jobs.find(id);
        if (i == jobs.end()) throw std::runtime_error("Torrent is not loaded");
        return i->second;
    }
    void save(Job& j, bool modified_only = false) {
        ++j.pending_saves;
        auto flags = lt::torrent_handle::save_info_dict | lt::torrent_handle::flush_disk_cache;
        if (modified_only) flags |= lt::torrent_handle::only_if_modified;
        j.handle.save_resume_data(flags);
    }
    template<class Predicate> void wait(Predicate done) {
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(15);
        while (!done()) {
            if (std::chrono::steady_clock::now() >= deadline) throw std::runtime_error("Timed out waiting for torrent storage. Retry the operation.");
            session->wait_for_alert(std::chrono::milliseconds(100));
            drain();
        }
    }
    void drain() {
        if (!session) return;
        std::vector<lt::alert*> alerts;
        session->pop_alerts(&alerts);
        bool retry_listen = false;
        for (auto a : alerts) {
#ifdef _WIN32
            // Windows reserves different TCP and UDP ranges. An automatic TCP
            // port can be forbidden to UDP; retry without changing interfaces.
            if (auto failed = lt::alert_cast<lt::listen_failed_alert>(a)) {
                bool automatic = listen_request.find(":0,") != std::string::npos
                    || (listen_request.size() >= 2 && listen_request.compare(listen_request.size() - 2, 2, ":0") == 0);
                if (automatic && failed->op == lt::operation_t::sock_bind
                    && (failed->error.value() == WSAEACCES || failed->error.value() == WSAEADDRINUSE)) retry_listen = true;
            }
#endif
            if (auto updated = lt::alert_cast<lt::state_update_alert>(a)) {
                for (auto const& s : updated->status) {
                    auto owner = owners.find(s.handle);
                    if (owner != owners.end()) { auto& job = jobs.at(owner->second); job.cached = s; job.cached_at = std::chrono::steady_clock::now(); }
                }
                continue;
            }
            auto ta = dynamic_cast<lt::torrent_alert*>(a);
            auto owner = ta ? owners.find(ta->handle) : owners.end();
            auto it = owner != owners.end() ? jobs.find(owner->second) : jobs.end();
            if (it == jobs.end()) {
                session_activity.push_back({{"time", std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count()}, {"message", a->message()}});
                if (session_activity.size() > 100) session_activity.pop_front();
                continue;
            }
            auto& j = it->second;
            if (j.removing) {
                // Alerts queued before removal can outlive the native handle.
                if (lt::alert_cast<lt::torrent_removed_alert>(a)) j.removed = true;
                continue;
            }
            if (!lt::alert_cast<lt::save_resume_data_alert>(a)) {
                j.activity.push_back({{"time", std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count()}, {"message", a->message()}});
                if (j.activity.size() > 100) j.activity.pop_front();
            }
            try {
                if (lt::alert_cast<lt::torrent_finished_alert>(a)) {
                    j.flushed = false; j.flushing = true; j.handle.flush_cache();
                } else if (lt::alert_cast<lt::cache_flushed_alert>(a)) {
                    j.flushed = true; j.flushing = false;
                } else if (auto s = lt::alert_cast<lt::save_resume_data_alert>(a)) {
                    j.pending_saves = std::max(0, j.pending_saves - 1);
                    write_file(j.resume_path, lt::write_resume_data_buf(s->params));
                } else if (lt::alert_cast<lt::save_resume_data_failed_alert>(a)) {
                    j.pending_saves = std::max(0, j.pending_saves - 1);
                } else if (lt::alert_cast<lt::torrent_removed_alert>(a)) {
                    j.removed = true;
                } else if (lt::alert_cast<lt::metadata_received_alert>(a)) {
                    auto ti = j.handle.torrent_file();
                    auto m = metadata(*ti);
                    lt::add_torrent_params p; p.ti = std::const_pointer_cast<lt::torrent_info>(ti);
                    for (auto const& tracker : j.handle.trackers()) { p.trackers.push_back(tracker.url); p.tracker_tiers.push_back(tracker.tier); }
                    for (auto const& seed : j.handle.url_seeds()) p.url_seeds.push_back(seed);
                    write_file(j.metadata_path, lt::write_torrent_file_buf(p, lt::write_flags::allow_missing_piece_layer));
                    events.push_back({{"id", it->first}, {"type", "metadata"}, {"metadata", m}});
                } else if (auto e = lt::alert_cast<lt::torrent_error_alert>(a)) {
                    j.error = e->error.message();
                    events.push_back({{"id", it->first}, {"type", "error"}, {"message", j.error}});
                } else if (lt::alert_cast<lt::storage_moved_alert>(a)) {
                    j.moving = false;
                    release(j.paths); j.paths = std::move(j.moving_paths);
                    for (auto const& path : j.paths) path_owners[path] = it->first;
                    events.push_back({{"id", it->first}, {"type", "moved"}, {"path", j.handle.status().save_path}});
                } else if (auto e = lt::alert_cast<lt::storage_moved_failed_alert>(a)) {
                    j.moving = false;
                    release(j.moving_paths); j.moving_paths.clear();
                    for (auto const& path : j.paths) path_owners[path] = it->first;
                    events.push_back({{"id", it->first}, {"type", "error"}, {"message", e->error.message()}});
                }
            } catch (std::exception const& e) {
                j.handle.pause(); j.error = e.what();
                events.push_back({{"id", it->first}, {"type", "error"}, {"message", j.error}});
            }
        }
        if (retry_listen && listen_retries < 16 && session->listen_port() == 0) {
            ++listen_retries; session->reopen_network_sockets({});
        }
    }
    json call(json const& c) {
        auto op = c.at("op").get<std::string>();
        if (op == "init") { root = c.at("root"); return {{"version", LIBTORRENT_VERSION}}; }
        if (op == "inspect") {
            auto bytes = read_file(c.at("path"));
            auto p = lt::load_torrent_buffer(lt::span<char const>(bytes.data(), bytes.size()));
            return metadata(*p.ti);
        }
        if (op == "pathAvailable") return available(path_key(fs::u8path(c.at("path").get<std::string>())));
        if (op == "create") {
            auto files = lt::list_files(c.at("path").get<std::string>());
            auto flags = lt::create_flags_t{};
            if (c.value("format", "hybrid") == "v1") flags |= lt::create_torrent::v1_only;
            if (c.value("format", "hybrid") == "v2") flags |= lt::create_torrent::v2_only;
            lt::create_torrent creator(files, c.value("pieceLength", 0), flags);
            for (auto const& tracker : c.value("trackers", json::array())) creator.add_tracker(tracker.get<std::string>());
            creator.set_priv(c.value("private", false));
            lt::set_piece_hashes(creator, fs::u8path(c.at("path").get<std::string>()).parent_path().u8string());
            auto bytes = creator.generate_buf(); write_file(c.at("output"), bytes);
            return {{"path", c.at("output")}};
        }
        if (op == "poll") {
            drain(); json states = json::array();
            if (session) session->post_torrent_updates(lt::torrent_handle::query_accurate_download_counters);
            for (auto& pair : jobs) {
                if (pair.second.importing || pair.second.removing || !pair.second.cached) continue;
                auto const& s = *pair.second.cached;
                if (!s.is_finished) pair.second.flushed = false;
                if (s.is_finished && !pair.second.flushed && !pair.second.flushing) {
                    pair.second.flushing = true; pair.second.handle.flush_cache();
                }
                bool ready = s.is_finished && pair.second.flushed;
                auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(std::chrono::steady_clock::now() - pair.second.cached_at).count();
                if (s.flags & lt::torrent_flags::paused) elapsed = 0;
                std::string state = "downloading";
                if (s.flags & lt::torrent_flags::paused) state = "paused";
                else if (s.state == lt::torrent_status::checking_files || s.state == lt::torrent_status::checking_resume_data) state = "checking";
                else if (!s.has_metadata) state = "metadata";
                else if (ready) state = "seeding";
                else if (s.is_finished) state = "checking";
                else if (s.download_payload_rate == 0 && s.num_peers == 0) state = "stalled";
                if (s.errc) state = "failed";
                states.push_back({{"id", pair.first}, {"state", state}, {"wanted", s.total_wanted}, {"downloaded", s.total_wanted_done},
                    {"downloadSpeed", s.download_payload_rate}, {"uploadSpeed", s.upload_payload_rate}, {"uploaded", s.all_time_upload},
                    {"allDownloaded", s.all_time_download}, {"peers", s.num_peers}, {"seeds", s.num_seeds}, {"finished", ready},
                    {"activeSeconds", s.active_duration.count() + elapsed}, {"seedSeconds", s.finished_duration.count() + (s.is_finished ? elapsed : 0)},
                    {"verified", s.state != lt::torrent_status::checking_resume_data && s.state != lt::torrent_status::checking_files},
                    {"error", s.errc ? s.errc.message() : pair.second.error}});
            }
            json out = {{"states", states}, {"events", events}}; events.clear(); return out;
        }
        if (op == "add") {
            auto id = c.at("id").get<std::string>();
            if (jobs.count(id)) throw std::runtime_error("Torrent already loaded");
            lt::add_torrent_params p;
            auto resume = root + "/" + id + ".resume";
            auto metapath = root + "/" + id + ".torrent";
            if (c.value("restore", false) && fs::exists(fs::u8path(resume))) {
                try { p = lt::read_resume_data(read_file(resume)); } catch (...) { p = {}; }
            }
            auto source = c.at("source").get<std::string>();
            if (!p.ti) {
                if (source.rfind("magnet:", 0) == 0) p = lt::parse_magnet_uri(source);
                else { auto b = read_file(source); p = lt::load_torrent_buffer(lt::span<char const>(b.data(), b.size())); }
            }
            if (p.ti) metadata(*p.ti);
            auto h = p.ti ? p.ti->info_hashes() : p.info_hashes;
            for (auto const& pair : jobs) {
                if (pair.second.removing) continue;
                auto other = pair.second.handle.info_hashes();
                if ((h.has_v1() && other.has_v1() && h.v1 == other.v1) || (h.has_v2() && other.has_v2() && h.v2 == other.v2))
                    throw std::runtime_error("This torrent is already in Fetchrail");
            }
            bool importing = c.value("importing", false);
            p.save_path = c.at("destination");
            p.flags &= ~lt::torrent_flags::auto_managed;
            p.flags |= lt::torrent_flags::paused;
            if (importing) { p.flags |= lt::torrent_flags::upload_mode | lt::torrent_flags::default_dont_download; }
            else { p.flags &= ~(lt::torrent_flags::upload_mode | lt::torrent_flags::default_dont_download); }
            if (c.contains("priorities")) { p.file_priorities.clear(); for (auto const& v : c["priorities"]) p.file_priorities.push_back(lt::download_priority_t(v.get<int>())); }
            if (p.ti) {
                bool restoring = c.value("restore", false);
                bool keep_cache = restoring && fs::exists(fs::u8path(metapath));
                if (keep_cache) {
                    try { auto bytes = read_file(metapath); lt::load_torrent_buffer(lt::span<char const>(bytes.data(), bytes.size())); }
                    catch (...) { keep_cache = false; }
                }
                if (!keep_cache) {
                    auto cached = p;
                    // Resume trees can be sparse or contain only wanted files.
                    // They belong in .resume, never as incomplete v2 piece layers.
                    if (restoring) { cached.merkle_trees.clear(); cached.merkle_tree_mask.clear(); cached.verified_leaf_hashes.clear(); }
                    write_file(metapath, lt::write_torrent_file_buf(cached, lt::write_flags::allow_missing_piece_layer));
                }
            }
            auto handle = get_session(c.value("config", json::object())).add_torrent(p);
            jobs.emplace(id, Job{handle, importing, resume, metapath});
            owners.emplace(handle, id);
            try { if (p.ti && !importing) jobs.at(id).paths = claim(id, p.save_path, metadata(*p.ti)); }
            catch (...) { session->remove_torrent(handle); owners.erase(handle); jobs.erase(id); throw; }
            if (importing || c.value("start", false)) handle.resume();
            return {{"hashes", hashes(handle.info_hashes())}, {"metadata", p.ti ? metadata(*p.ti) : json(nullptr)}};
        }
        if (op == "configure") {
            if (!session) return json::object();
            auto requested = c.value("listenInterfaces", "0.0.0.0:0,[::]:0");
            if (requested != listen_request) { listen_request = requested; listen_retries = 0; }
            auto p = network_settings(c);
            p.set_int(lt::settings_pack::download_rate_limit, c.value("downloadLimit", 0));
            p.set_int(lt::settings_pack::upload_rate_limit, c.value("uploadLimit", 0));
            session->apply_settings(p); return json::object();
        }
        if (op == "checkpoint") {
            for (auto& pair : jobs) if (!pair.second.importing && !pair.second.removing && !pair.second.pending_saves) save(pair.second, true);
            if (session && c.value("wait", false)) wait([&] { for (auto const& pair : jobs) if (pair.second.pending_saves) return false; return true; });
            return json::object();
        }
        auto id = c.at("id").get<std::string>();
        if (op == "remove" && !jobs.count(id)) return json::object();
        auto& j = job(id); auto h = j.handle;
        if (j.removing && op != "remove") throw std::runtime_error("Torrent removal is pending. Retry removal.");
        if (op == "pause") { h.pause(); save(j); }
        else if (op == "resume") { j.error.clear(); h.resume(); }
        else if (op == "recheck") { j.flushed = false; h.force_recheck(); }
        else if (op == "limit") { h.set_download_limit(c.value("downloadLimit", 0)); h.set_upload_limit(c.value("uploadLimit", 0)); }
        else if (op == "sequential") { if (c.at("enabled").get<bool>()) h.set_flags(lt::torrent_flags::sequential_download); else h.unset_flags(lt::torrent_flags::sequential_download); }
        else if (op == "priorities") {
            auto ti = h.torrent_file(); if (!ti) throw std::runtime_error("Metadata is not ready");
            std::vector<lt::download_priority_t> priorities; for (auto const& v : c.at("priorities")) priorities.push_back(lt::download_priority_t(v.get<int>()));
            if (priorities.size() != size_t(ti->num_files())) throw std::runtime_error("File selection does not match torrent");
            j.flushed = false;
            h.prioritize_files(priorities);
        } else if (op == "move") {
            if (j.moving) throw std::runtime_error("Storage is already moving");
            j.moving_paths = claim(id, c.at("path"), metadata(*h.torrent_file()));
            j.moving = true; h.move_storage(c.at("path"));
        }
        else if (op == "announce") { h.force_reannounce(); h.force_dht_announce(); }
        else if (op == "tracker") {
            lt::announce_entry entry(c.at("url").get<std::string>());
            auto scheme = entry.url.substr(0, entry.url.find(':'));
            if (scheme != "http" && scheme != "https" && scheme != "udp") throw std::runtime_error("Tracker must use HTTP, HTTPS or UDP");
            h.add_tracker(entry);
        } else if (op == "peer") {
            auto port = c.at("port").get<int>(); if (port < 1 || port > 65535) throw std::runtime_error("Peer port must be 1–65535");
            h.connect_peer({lt::make_address(c.at("address").get<std::string>()), static_cast<unsigned short>(port)});
        }
        else if (op == "removeTracker") {
            auto trackers = h.trackers(); auto url = c.at("url").get<std::string>();
            trackers.erase(std::remove_if(trackers.begin(), trackers.end(), [&](auto const& tracker) { return tracker.url == url; }), trackers.end());
            h.replace_trackers(trackers); save(j);
        }
        else if (op == "remove") {
            if (!j.removing) {
                h.pause();
                wait([&] { return j.pending_saves == 0; });
                j.removing = true; j.flushed = false; j.flushing = true;
                session->remove_torrent(h);
            }
            // 2.1 emits torrent_removed_alert after async_stop_torrent closes storage.
            wait([&] { return j.removed; });
            release(j.paths); release(j.moving_paths);
            owners.erase(h); jobs.erase(id);
        }
        else if (op == "details") {
            if (j.importing && !j.error.empty()) throw std::runtime_error(j.error);
            json out = {{"metadata", nullptr}, {"peers", json::array()}, {"trackers", json::array()}, {"port", session->listen_port()}, {"moving", j.moving},
                {"sequential", bool(h.flags() & lt::torrent_flags::sequential_download)}, {"downloadLimit", h.download_limit()}};
            auto view = c.value("view", "all");
            if (view == "activity" || view == "all") {
                out["activity"] = session_activity;
                for (auto const& entry : j.activity) out["activity"].push_back(entry);
            }
            auto ti = h.torrent_file();
            if (ti && (view == "all" || view == "files")) {
                auto m = metadata(*ti); auto priorities = h.get_file_priorities(); std::vector<std::int64_t> progress;
                h.file_progress(progress, lt::torrent_handle::piece_granularity);
                for (auto& f : m["files"]) { int i = f["index"]; f["priority"] = int(static_cast<std::uint8_t>(priorities[i])); f["downloaded"] = progress[i]; }
                out["metadata"] = m;
            }
            std::vector<lt::peer_info> peers; if (view == "all" || view == "peers") h.get_peer_info(peers);
            for (auto const& p : peers) out["peers"].push_back({{"address", p.remote_endpoint().address().to_string()}, {"port", p.remote_endpoint().port()}, {"client", p.client}, {"downloadSpeed", p.payload_down_speed}, {"uploadSpeed", p.payload_up_speed}, {"progress", p.progress}});
            for (auto const& t : (view == "all" || view == "trackers" ? h.trackers() : std::vector<lt::announce_entry>{})) {
                std::string error; int failures = 0, complete = -1, incomplete = -1;
                for (auto const& endpoint : t.endpoints) for (auto const& info : endpoint.info_hashes) {
                    if (info.last_error) error = info.last_error.message();
                    if (!info.message.empty()) error = info.message;
                    failures = std::max(failures, int(info.fails));
                    complete = std::max(complete, info.scrape_complete); incomplete = std::max(incomplete, info.scrape_incomplete);
                }
                out["trackers"].push_back({{"url", t.url}, {"tier", t.tier}, {"verified", t.verified}, {"error", error}, {"failures", failures}, {"seeds", complete}, {"peers", incomplete}});
            }
            return out;
        } else throw std::runtime_error("Unknown torrent command");
        return json::object();
    }
};

extern "C" void* fr_torrent_new() noexcept { try { return new Engine(); } catch (...) { return nullptr; } }
extern "C" void fr_torrent_drop(void* ptr) noexcept { delete static_cast<Engine*>(ptr); }
extern "C" void fr_torrent_free(char* ptr) noexcept { std::free(ptr); }
extern "C" char* fr_torrent_call(void* ptr, char const* input) noexcept {
    std::string output;
    try { output = json({{"ok", static_cast<Engine*>(ptr)->call(json::parse(input))}}).dump(); }
    catch (std::exception const& e) { output = json({{"error", e.what()}}).dump(); }
    catch (...) { output = "{\"error\":\"Native torrent engine failed\"}"; }
    auto result = static_cast<char*>(std::malloc(output.size() + 1));
    if (result) std::memcpy(result, output.c_str(), output.size() + 1);
    return result;
}
