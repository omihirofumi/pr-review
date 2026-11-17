/// Uses FNV-1a rather than `DefaultHasher`, whose output may change between Rust releases and
/// orphan existing entries; collisions are harmless because readers compare the stored identity.
pub fn file_name(identity: &str) -> String {
    let hash = identity
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{hash:016x}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_published_fnv1a_vectors() {
        assert_eq!(file_name(""), "cbf29ce484222325.json");
        assert_eq!(file_name("a"), "af63dc4c8601ec8c.json");
        assert_eq!(file_name("foobar"), "85944171f73967e8.json");
    }
}
