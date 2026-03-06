use super::FsError;

pub const MAX_PATH_BYTES: usize = 160;
const MAX_COMPONENTS: usize = 16;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum MountKind {
    Root,
    Proc,
    Tmp,
}

impl MountKind {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Root => "/",
            Self::Proc => "/proc",
            Self::Tmp => "/tmp",
        }
    }

    pub const fn writable(self) -> bool {
        match self {
            Self::Root | Self::Tmp => true,
            Self::Proc => false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct MountInfo {
    pub kind: MountKind,
    pub path: &'static str,
}

pub const MOUNTS: [MountInfo; 3] = [
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
    let trimmed = path.trim();
    let input = if trimmed.is_empty() { "/" } else { trimmed };

    let mut components = [""; MAX_COMPONENTS];
    let mut depth = 0usize;

    for component in input.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth >= MAX_COMPONENTS {
            return Err(FsError::InvalidPath);
        }
        components[depth] = component;
        depth += 1;
    }

    let mut bytes = [0u8; MAX_PATH_BYTES];
    if depth == 0 {
        bytes[0] = b'/';
        return Ok(CanonicalPath { bytes, len: 1 });
    }

    let mut len = 0usize;
    for component in components.iter().take(depth) {
        let name = component.as_bytes();
        if len >= MAX_PATH_BYTES || len + 1 + name.len() > MAX_PATH_BYTES {
            return Err(FsError::InvalidPath);
        }
        bytes[len] = b'/';
        len += 1;
        bytes[len..len + name.len()].copy_from_slice(name);
        len += name.len();
    }

    Ok(CanonicalPath { bytes, len })
}

pub fn resolve_mount(path: &CanonicalPath) -> ResolvedMountPath {
    let canonical = path.as_str();
    let kind = if matches_mount(canonical, MountKind::Proc.path()) {
        MountKind::Proc
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
