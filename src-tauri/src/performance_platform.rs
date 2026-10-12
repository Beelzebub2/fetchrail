use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Hardware {
    pub processor_model: Option<String>,
    pub os: String,
    pub architecture: String,
    pub logical_processors: usize,
    pub memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub process_memory_bytes: Option<u64>,
    pub on_battery: Option<bool>,
    pub metered: Option<bool>,
    pub adapters: Vec<Adapter>,
    pub network_key: String,
    pub drive_key: String,
    pub free_disk_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub filesystem: Option<String>,
    pub disk_bus: Option<String>,
    pub disk_model: Option<String>,
    pub disk_seek_penalty: Option<bool>,
    pub disk_trim: Option<bool>,
    pub process_https_connections: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Adapter {
    pub kind: String,
    pub receive_link_bps: u64,
    pub transmit_link_bps: u64,
    pub mtu: u32,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub receive_errors: u64,
    pub receive_discards: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Counters {
    pub idle: u64,
    pub total: u64,
    pub process: u64,
    pub tcp_retransmissions: Option<u64>,
}

pub fn key(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub fn cpu_usage(before: &Counters, after: &Counters) -> Option<f64> {
    let total = after.total.checked_sub(before.total)?;
    let idle = after.idle.checked_sub(before.idle)?;
    (total > 0).then(|| 100.0 * total.saturating_sub(idle) as f64 / total as f64)
}

pub fn snapshot(path: &Path) -> (Hardware, Counters) {
    let mut hardware = Hardware {
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        logical_processors: std::thread::available_parallelism().map_or(1, usize::from),
        drive_key: key(&path.to_string_lossy()),
        ..Hardware::default()
    };
    let mut counters = Counters::default();
    collect(path, &mut hardware, &mut counters);
    // Never retain adapter names, MACs, SSIDs, addresses, proxy URLs or credentials.
    hardware.network_key = key(&format!(
        "{}:{:?}:{}",
        hardware.network_key,
        hardware.metered,
        [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "NO_PROXY",
            "no_proxy"
        ]
        .iter()
        .map(|name| std::env::var_os(name)
            .map(|v| key(&v.to_string_lossy()))
            .unwrap_or_default())
        .collect::<Vec<_>>()
        .join(":")
    ));
    (hardware, counters)
}

pub fn inventory(path: &Path) -> (Hardware, Counters) {
    let (mut h, c) = snapshot(path);
    #[cfg(windows)]
    inventory_windows(path, &mut h);
    #[cfg(target_os = "linux")]
    {
        h.processor_model = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|v| {
            v.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| {
                    l.split_once(':')
                        .map(|(_, v)| v.trim().chars().take(120).collect())
                })
        });
    }
    (h, c)
}

pub fn process_cpu(
    before: &Counters,
    after: &Counters,
    elapsed: std::time::Duration,
    processors: usize,
) -> Option<f64> {
    #[cfg(windows)]
    let ticks = 10_000_000.0;
    #[cfg(target_os = "linux")]
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    #[cfg(not(any(windows, target_os = "linux")))]
    let ticks = 0.0;
    if ticks <= 0.0 || elapsed.is_zero() {
        return None;
    }
    Some(
        (100.0 * after.process.checked_sub(before.process)? as f64
            / ticks
            / elapsed.as_secs_f64()
            / processors.max(1) as f64)
            .clamp(0.0, 100.0),
    )
}

pub fn volume_key(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{GetVolumeNameForVolumeMountPointW, GetVolumePathNameW},
        };
        let path: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut root = [0u16; 1024];
        let mut id = [0u16; 1024];
        unsafe {
            GetVolumePathNameW(PCWSTR(path.as_ptr()), &mut root).ok()?;
            GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut id).ok()?;
        }
        return Some(key(&String::from_utf16_lossy(
            &id[..id.iter().position(|v| *v == 0).unwrap_or(id.len())],
        )));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        return std::fs::metadata(path)
            .ok()
            .map(|v| key(&v.dev().to_string()));
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn collect(path: &Path, h: &mut Hardware, c: &mut Counters) {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::FILETIME,
            NetworkManagement::{
                IpHelper::{FreeMibTable, GetIfTable2, GetTcpStatisticsEx, MIB_TCPSTATS_LH},
                Ndis::IfOperStatusUp,
            },
            Storage::FileSystem::{
                GetDiskFreeSpaceExW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
                GetVolumePathNameW,
            },
            System::{
                Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
                SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
                Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes},
            },
        },
    };
    let ticks = |v: FILETIME| (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime);
    unsafe {
        let mut memory = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut memory).is_ok() {
            h.memory_bytes = Some(memory.ullTotalPhys);
            h.available_memory_bytes = Some(memory.ullAvailPhys);
        }
        let mut process_memory = PROCESS_MEMORY_COUNTERS::default();
        if GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut process_memory,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .is_ok()
        {
            h.process_memory_bytes = Some(process_memory.WorkingSetSize as u64);
        }
        let (mut idle, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
            c.idle = ticks(idle);
            c.total = ticks(kernel).saturating_add(ticks(user));
        }
        let (mut created, mut exited) = (FILETIME::default(), FILETIME::default());
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .is_ok()
        {
            c.process = ticks(kernel).saturating_add(ticks(user));
        }
        let mut power = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut power).is_ok() && power.ACLineStatus < 2 {
            h.on_battery = Some(power.ACLineStatus == 0);
        }
        // windows-rs keeps its cached activation factories' MTA alive across snapshots.
        if let Ok(profile) =
            windows::Networking::Connectivity::NetworkInformation::GetInternetConnectionProfile()
        {
            if let Ok(cost) = profile
                .GetConnectionCost()
                .and_then(|v| v.NetworkCostType())
            {
                use windows::Networking::Connectivity::NetworkCostType;
                if cost != NetworkCostType::Unknown {
                    h.metered = Some(cost != NetworkCostType::Unrestricted);
                }
            }
        }
        let mut table = std::ptr::null_mut();
        let mut identities = Vec::new();
        if GetIfTable2(&mut table).is_ok() && !table.is_null() {
            let rows = std::slice::from_raw_parts(
                std::ptr::addr_of!((*table).Table)
                    .cast::<windows::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2>(),
                (*table).NumEntries as usize,
            );
            for row in rows
                .iter()
                .filter(|r| r.OperStatus == IfOperStatusUp && r.Type != 24)
                .take(32)
            {
                identities.push(format!(
                    "{}:{:?}:{}",
                    row.InterfaceLuid.Value, row.NetworkGuid, row.Type
                ));
                h.adapters.push(Adapter {
                    kind: match row.Type {
                        6 => "ethernet",
                        71 => "wifi",
                        131 => "tunnel",
                        _ => "other",
                    }
                    .into(),
                    receive_link_bps: row.ReceiveLinkSpeed,
                    transmit_link_bps: row.TransmitLinkSpeed,
                    mtu: row.Mtu,
                    received_bytes: row.InOctets,
                    sent_bytes: row.OutOctets,
                    receive_errors: row.InErrors,
                    receive_discards: row.InDiscards,
                });
            }
            FreeMibTable(table.cast());
        }
        identities.sort();
        let proxy = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            .ok()
            .map(|k| {
                format!(
                    "{}:{}:{}",
                    k.get_value::<u32, _>("ProxyEnable").unwrap_or(0),
                    key(&k.get_value::<String, _>("ProxyServer").unwrap_or_default()),
                    key(&k
                        .get_value::<String, _>("AutoConfigURL")
                        .unwrap_or_default())
                )
            })
            .unwrap_or_default();
        h.network_key = format!("{}:{}", identities.join(";"), proxy);
        h.process_https_connections = https_connections();
        let mut retrans = 0u64;
        let mut available = false;
        for family in [2, 23] {
            let mut tcp = MIB_TCPSTATS_LH::default();
            if GetTcpStatisticsEx(&mut tcp, family) == 0 {
                available = true;
                retrans += u64::from(tcp.dwRetransSegs);
            }
        }
        c.tcp_retransmissions = available.then_some(retrans);
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let (mut free, mut total) = (0, 0);
        if GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free),
            Some(&mut total),
            None,
        )
        .is_ok()
        {
            h.free_disk_bytes = Some(free);
            h.disk_bytes = Some(total);
        }
        let mut root = [0u16; 1024];
        if GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root).is_ok() {
            let mut identity = [0u16; 1024];
            if GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut identity).is_ok() {
                h.drive_key = key(&String::from_utf16_lossy(
                    &identity[..identity
                        .iter()
                        .position(|v| *v == 0)
                        .unwrap_or(identity.len())],
                ));
            }
            let mut fs = [0u16; 128];
            if GetVolumeInformationW(PCWSTR(root.as_ptr()), None, None, None, None, Some(&mut fs))
                .is_ok()
            {
                h.filesystem = Some(String::from_utf16_lossy(
                    &fs[..fs.iter().position(|v| *v == 0).unwrap_or(fs.len())],
                ));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn collect(path: &Path, h: &mut Hardware, c: &mut Counters) {
    use std::{
        fs,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
    };
    if let Ok(text) = fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let name = fields.next().unwrap_or_default();
            let bytes = fields
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.saturating_mul(1024));
            match name {
                "MemTotal:" => h.memory_bytes = bytes,
                "MemAvailable:" => h.available_memory_bytes = bytes,
                _ => {}
            }
        }
    }
    if let Ok(text) = fs::read_to_string("/proc/stat") {
        let values: Vec<u64> = text
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .take(8)
            .filter_map(|v| v.parse().ok())
            .collect();
        c.total = values.iter().sum();
        c.idle = values.get(3).copied().unwrap_or(0) + values.get(4).copied().unwrap_or(0);
    }
    if let Ok(text) = fs::read_to_string("/proc/self/stat") {
        if let Some((_, rest)) = text.rsplit_once(')') {
            let values: Vec<&str> = rest.split_whitespace().collect();
            c.process = [11, 12]
                .iter()
                .filter_map(|i| values.get(*i)?.parse::<u64>().ok())
                .sum();
        }
    }
    if let Ok(text) = fs::read_to_string("/proc/self/status") {
        h.process_memory_bytes = text
            .lines()
            .find(|v| v.starts_with("VmRSS:"))
            .and_then(|v| v.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|v| v * 1024);
    }
    if let Ok(text) = fs::read_to_string("/proc/net/snmp") {
        let lines: Vec<&str> = text.lines().filter(|v| v.starts_with("Tcp:")).collect();
        if lines.len() >= 2 {
            if let Some(index) = lines[0].split_whitespace().position(|v| v == "RetransSegs") {
                c.tcp_retransmissions = lines[1]
                    .split_whitespace()
                    .nth(index)
                    .and_then(|v| v.parse().ok());
            }
        }
    }
    let mut identities = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten().take(64) {
            let dir = entry.path();
            if entry.file_name() == "lo"
                || fs::read_to_string(dir.join("operstate"))
                    .ok()
                    .is_none_or(|v| v.trim() != "up")
            {
                continue;
            }
            let number = |name: &str| {
                fs::read_to_string(dir.join(name))
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
            };
            identities.push(format!(
                "{}:{}",
                entry.file_name().to_string_lossy(),
                number("ifindex").unwrap_or(0)
            ));
            let speed = number("speed").unwrap_or(0).saturating_mul(1_000_000);
            h.adapters.push(Adapter {
                kind: if dir.join("wireless").exists() {
                    "wifi"
                } else if dir.join("device").exists() {
                    "ethernet"
                } else {
                    "tunnel"
                }
                .into(),
                receive_link_bps: speed,
                transmit_link_bps: speed,
                mtu: number("mtu").unwrap_or(0) as u32,
                received_bytes: number("statistics/rx_bytes").unwrap_or(0),
                sent_bytes: number("statistics/tx_bytes").unwrap_or(0),
                receive_errors: number("statistics/rx_errors").unwrap_or(0),
                receive_discards: number("statistics/rx_dropped").unwrap_or(0),
            });
        }
    }
    identities.sort();
    h.network_key = identities.join(";");
    if let Ok(metadata) = fs::metadata(path) {
        h.drive_key = key(&metadata.dev().to_string());
    }
    if let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } == 0 {
            let info = unsafe { info.assume_init() };
            h.free_disk_bytes = Some(info.f_bavail.saturating_mul(info.f_frsize));
            h.disk_bytes = Some(info.f_blocks.saturating_mul(info.f_frsize));
        }
    }
}

#[cfg(windows)]
fn https_connections() -> Option<usize> {
    use windows::Win32::{
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
            TCP_TABLE_OWNER_PID_ALL,
        },
        System::Threading::GetCurrentProcessId,
    };
    let mut count = 0;
    unsafe {
        let pid = GetCurrentProcessId();
        for family in [2, 23] {
            let mut size = 0;
            let error =
                GetExtendedTcpTable(None, &mut size, false, family, TCP_TABLE_OWNER_PID_ALL, 0);
            if !matches!(error, 0 | 122) || size == 0 || size > 1024 * 1024 {
                return None;
            }
            let mut data = vec![0u64; (size as usize).div_ceil(8)];
            if GetExtendedTcpTable(
                Some(data.as_mut_ptr().cast()),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            ) != 0
            {
                return None;
            }
            if family == 2 {
                let table = data.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
                let rows = (*table).dwNumEntries as usize;
                if 4 + rows.checked_mul(std::mem::size_of::<
                    windows::Win32::NetworkManagement::IpHelper::MIB_TCPROW_OWNER_PID,
                >())?
                    > size as usize
                {
                    return None;
                }
                for row in
                    std::slice::from_raw_parts(std::ptr::addr_of!((*table).table).cast::<windows::Win32::NetworkManagement::IpHelper::MIB_TCPROW_OWNER_PID>(), rows)
                {
                    if row.dwOwningPid == pid
                        && row.dwState == 5
                        && u16::from_be(row.dwRemotePort as u16) == 443
                    {
                        count += 1;
                    }
                }
            } else {
                let table = data.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
                let rows = (*table).dwNumEntries as usize;
                if 4 + rows.checked_mul(std::mem::size_of::<
                    windows::Win32::NetworkManagement::IpHelper::MIB_TCP6ROW_OWNER_PID,
                >())?
                    > size as usize
                {
                    return None;
                }
                for row in
                    std::slice::from_raw_parts(std::ptr::addr_of!((*table).table).cast::<windows::Win32::NetworkManagement::IpHelper::MIB_TCP6ROW_OWNER_PID>(), rows)
                {
                    if row.dwOwningPid == pid
                        && row.dwState == 5
                        && u16::from_be(row.dwRemotePort as u16) == 443
                    {
                        count += 1;
                    }
                }
            }
        }
    }
    Some(count)
}

#[cfg(windows)]
fn inventory_windows(path: &Path, h: &mut Hardware) {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::GetVolumePathNameW,
            System::{
                Ioctl::{
                    PropertyStandardQuery, StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
                    StorageDeviceTrimProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
                    DEVICE_TRIM_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
                    STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY,
                },
                IO::DeviceIoControl,
            },
        },
    };
    h.processor_model = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0")
        .ok()
        .and_then(|k| k.get_value::<String, _>("ProcessorNameString").ok())
        .map(|v| v.trim().chars().take(120).collect());
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut root = [0u16; 1024];
    if unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root) }.is_err() {
        return;
    }
    let root =
        String::from_utf16_lossy(&root[..root.iter().position(|v| *v == 0).unwrap_or(root.len())]);
    let root = root.strip_prefix("\\\\?\\").unwrap_or(&root);
    if root.len() != 3 || !root.as_bytes()[0].is_ascii_alphabetic() || !root.ends_with(":\\") {
        return;
    }
    let device = format!("\\\\.\\{}", root.trim_end_matches('\\'));
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(0)
        .share_mode(3)
        .open(device)
    else {
        return;
    };
    fn query<T: Default>(file: &std::fs::File, property: STORAGE_PROPERTY_ID) -> Option<T> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: property,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut result = T::default();
        let mut returned = 0;
        unsafe {
            DeviceIoControl(
                HANDLE(file.as_raw_handle()),
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some((&mut result as *mut T).cast()),
                std::mem::size_of::<T>() as u32,
                Some(&mut returned),
                None,
            )
        }
        .ok()?;
        (returned as usize >= std::mem::size_of::<T>()).then_some(result)
    }
    h.disk_seek_penalty =
        query::<DEVICE_SEEK_PENALTY_DESCRIPTOR>(&file, StorageDeviceSeekPenaltyProperty)
            .map(|v| v.IncursSeekPenalty);
    h.disk_trim =
        query::<DEVICE_TRIM_DESCRIPTOR>(&file, StorageDeviceTrimProperty).map(|v| v.TrimEnabled);
    if let Some(device) = query::<STORAGE_DEVICE_DESCRIPTOR>(&file, StorageDeviceProperty) {
        h.disk_bus = Some(
            match device.BusType.0 {
                7 => "USB",
                10 => "SAS",
                11 => "SATA",
                14 | 15 => "Virtual",
                16 => "Storage Spaces",
                17 => "NVMe",
                _ => "Other",
            }
            .into(),
        );
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn collect(_: &Path, _: &mut Hardware, _: &mut Counters) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_deltas_require_valid_intervals() {
        assert_eq!(
            cpu_usage(
                &Counters::default(),
                &Counters {
                    idle: 30,
                    total: 100,
                    ..Default::default()
                }
            ),
            Some(70.0)
        );
        assert_eq!(cpu_usage(&Counters::default(), &Counters::default()), None);
        assert_eq!(key("private-path").len(), 64);
        assert!(!key("private-path").contains("private"));
    }
}
