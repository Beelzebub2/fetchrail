use std::{
    fs::File,
    io,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
};
use windows::{
    Wdk::Storage::FileSystem::{
        FileLinkInformation, NtSetInformationFile, RtlNtStatusToDosErrorNoTeb,
        FILE_LINK_INFORMATION,
    },
    Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

// Link from the protected file handle: a path-based hard link could select a replaced source.
pub(super) fn link_verified(file: &File, destination: &Path) -> io::Result<Option<File>> {
    let path: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let mut name: Vec<u16> = r"\??\".encode_utf16().collect();
    if path.starts_with(&[92, 92, 63, 92]) {
        name.extend_from_slice(&path[4..]);
    } else if path.starts_with(&[92, 92]) {
        name.extend("UNC\\".encode_utf16());
        name.extend_from_slice(&path[2..]);
    } else {
        name.extend_from_slice(&path);
    }
    let bytes = std::mem::size_of::<FILE_LINK_INFORMATION>()
        .checked_add(
            name.len()
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("Publication name is too long"))?,
        )
        .ok_or_else(|| io::Error::other("Publication name is too long"))?;
    let length =
        u32::try_from(bytes).map_err(|_| io::Error::other("Publication name is too long"))?;
    let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    let mut status = IO_STATUS_BLOCK::default();
    unsafe {
        let info = storage.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).FileNameLength = u32::try_from(name.len() * 2)
            .map_err(|_| io::Error::other("Publication name is too long"))?;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        let result = NtSetInformationFile(
            HANDLE(file.as_raw_handle()),
            &mut status,
            info.cast(),
            length,
            FileLinkInformation,
        );
        if result.is_err() {
            let code = RtlNtStatusToDosErrorNoTeb(result) as i32;
            // Fall back only when creation itself is unsupported, before a final name exists.
            if matches!(code, 1 | 50 | 87 | 123 | 1142) {
                return Ok(None);
            }
            return Err(io::Error::from_raw_os_error(code));
        }
    }
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    // Write sharing accommodates our existing data guard; no delete sharing pins
    // this separate directory entry. The data guard still denies all new writers.
    let entry = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(destination)?;
    let mut original = BY_HANDLE_FILE_INFORMATION::default();
    let mut published = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut original)
            .map_err(io::Error::other)?;
        GetFileInformationByHandle(HANDLE(entry.as_raw_handle()), &mut published)
            .map_err(io::Error::other)?;
    }
    if original.dwVolumeSerialNumber != published.dwVolumeSerialNumber
        || original.nFileIndexHigh != published.nFileIndexHigh
        || original.nFileIndexLow != published.nFileIndexLow
        || published.dwFileAttributes & 0x400 != 0
    {
        return Err(io::Error::other(
            "Published directory entry changed; staging was preserved",
        ));
    }
    Ok(Some(entry))
}

// Mark only the temporary name opened by this handle. Its final hard link survives;
// the temporary name disappears when the last duplicate closes after catalog ACK.
pub(super) fn file_identity(file: &File) -> io::Result<(u32, u64)> {
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    if info.dwFileAttributes & 0x400 != 0 {
        return Err(io::Error::other(
            "Private publication alias is a reparse point",
        ));
    }
    Ok((
        info.dwVolumeSerialNumber,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}

pub(super) fn retire_named_entry(path: &Path, expected: (u32, u64)) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };
    let file = match std::fs::OpenOptions::new()
        .access_mode(DELETE.0 | FILE_READ_ATTRIBUTES.0)
        .share_mode(7)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if file_identity(&file)? != expected {
        return Err(io::Error::other(
            "Private publication alias changed; file preserved",
        ));
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(io::Error::other)
}
