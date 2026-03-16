// kernel/src/doom_bridge.rs: WAD embedding only (M32 — kernel Doom engine removed).

mod wad_embed {
    include!(concat!(env!("OUT_DIR"), "/doom_wad_embed.rs"));
}

/// Returns the embedded WAD bytes (empty if doom was built without a WAD file).
pub fn wad_bytes() -> &'static [u8] {
    wad_embed::ARROST_DOOM_WAD_BYTES
}
