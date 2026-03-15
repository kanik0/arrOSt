use super::FsError;

pub const MAX_PATH_BYTES: usize = 160;
const MAX_COMPONENTS: usize = 16;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum MountKind {
    Root,
    Proc,
    Tmp,
    Dev,
}

impl MountKind {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Root => "/",
            Self::Proc => "/proc",
            Self::Tmp => "/tmp",
            Self::Dev => "/dev",
        }
    }

    pub const fn writable(self) -> bool {
        match self {
            Self::Root | Self::Tmp => true,
            Self::Proc | Self::Dev => false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct MountInfo {
    pub kind: MountKind,
    pub path: &'static str,
}

pub const MOUNTS: [MountInfo; 4] = [
    MountInfo {
        kind: MountKind::Root,
        path: "/",
    },
    MountInfo {
        kind: MountKind::Proc,
        path: "/proc",
    },
    MountInfo {
        kind: MountKind::Tmp,
        path: "/tmp",
    },
    MountInfo {
        kind: MountKind::Dev,
        path: "/dev",
    },
];

#[derive(Clone, Copy)]
pub struct CanonicalPath {
    bytes: [u8; MAX_PATH_BYTES],
    len: usize,
}

impl CanonicalPath {
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("/")
    }
}

#[derive(Clone, Copy)]
pub struct ResolvedMountPath {
    pub kind: MountKind,
    local: [u8; MAX_PATH_BYTES],
    local_len: usize,
}

impl ResolvedMountPath {
    pub fn local_path(&self) -> &str {
        core::str::from_utf8(&self.local[..self.local_len]).unwrap_or("")
    }
}

pub fn canonicalize(path: &str) -> Result<CanonicalPath, FsError> {
    let path_bytes = path.as_bytes();
    let mut start = 0usize;
    let mut end = path_bytes.len();
    while start < end && path_bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && path_bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let input = if start == end {
        b"/".as_slice()
    } else {
        &path_bytes[start..end]
    };

    let mut component_starts = [0usize; MAX_COMPONENTS];
    let mut depth = 0usize;
    let mut bytes = [0u8; MAX_PATH_BYTES];
    let mut len = 0usize;

    let mut index = 0usize;
    while index < input.len() {
        while index < input.len() && input[index] == b'/' {
            index += 1;
        }
        if index >= input.len() {
            break;
        }

        let component_start = index;
        while index < input.len() && input[index] != b'/' {
            index += 1;
        }
        let component = &input[component_start..index];
        let component_len = component.len();
        if component_len == 1 && component[0] == b'.' {
            continue;
        }
        if component_len == 2 && component[0] == b'.' && component[1] == b'.' {
            if depth > 0 {
                depth -= 1;
                len = component_starts[depth];
            }
        } else {
            if depth >= MAX_COMPONENTS {
                return Err(FsError::InvalidPath);
            }
            if len + 1 + component_len > MAX_PATH_BYTES {
                return Err(FsError::InvalidPath);
            }
            component_starts[depth] = len;
            bytes[len] = b'/';
            len += 1;
            bytes[len..len + component_len].copy_from_slice(component);
            len += component_len;
            depth += 1;
        }
    }

    if depth == 0 {
        bytes[0] = b'/';
        return Ok(CanonicalPath { bytes, len: 1 });
    }

    Ok(CanonicalPath { bytes, len })
}

pub fn resolve_mount(path: &CanonicalPath) -> ResolvedMountPath {
    let canonical = path.as_str();
    let kind = if matches_mount(canonical, MountKind::Proc.path()) {
        MountKind::Proc
    } else if matches_mount(canonical, MountKind::Dev.path()) {
        MountKind::Dev
    } else if matches_mount(canonical, MountKind::Tmp.path()) {
        MountKind::Tmp
    } else {
        MountKind::Root
    };

    let local = match kind {
        MountKind::Root => canonical.strip_prefix('/').unwrap_or(canonical),
        _ if canonical == kind.path() => "",
        _ => canonical
            .strip_prefix(kind.path())
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(""),
    };

    let mut local_buf = [0u8; MAX_PATH_BYTES];
    let local_bytes = local.as_bytes();
    let local_len = local_bytes.len().min(MAX_PATH_BYTES);
    local_buf[..local_len].copy_from_slice(&local_bytes[..local_len]);

    ResolvedMountPath {
        kind,
        local: local_buf,
        local_len,
    }
}

fn matches_mount(path: &str, mount_path: &str) -> bool {
    path == mount_path
        || path
            .strip_prefix(mount_path)
            .is_some_and(|rest| rest.starts_with('/'))
}
