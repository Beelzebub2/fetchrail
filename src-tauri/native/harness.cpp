#include <iostream>
#include <string>
#include "json.hpp"
extern "C" void* fr_torrent_new() noexcept;
extern "C" void fr_torrent_drop(void*) noexcept;
extern "C" char* fr_torrent_call(void*, char const*) noexcept;
extern "C" void fr_torrent_free(char*) noexcept;
int main(int argc, char** argv) {
    if (argc != 2) return 2;
    auto engine = fr_torrent_new(); if (!engine) return 3;
    auto init = nlohmann::json({{"op", "init"}, {"root", argv[1]}}).dump();
    auto reply = fr_torrent_call(engine, init.c_str()); fr_torrent_free(reply);
    std::string request;
    while (std::getline(std::cin, request)) {
        reply = fr_torrent_call(engine, request.c_str());
        if (!reply) return 4;
        std::cout << reply << std::endl; fr_torrent_free(reply);
    }
    fr_torrent_drop(engine);
}
