//! Spec 009's opcode inventory vs. the harvested scope (#65, follow-up to
//! #58/#87). `tools/opcodes-v2.json` is the oracle-harvested 52-opcode set
//! that pinned `Opcode`'s variants in the first place; this test is the
//! machine-checked guarantee that the two never drift apart silently.

use std::collections::BTreeSet;

use sqlite_rs::vdbe::Opcode;

#[test]
fn opcode_inventory_matches_harvested_set() {
    let json = include_str!("../../tools/opcodes-v2.json");
    let harvested: BTreeSet<&str> = json
        .lines()
        .filter_map(|line| {
            // Top-level opcode entries are exactly 4-space-indented `"Name": {`
            // keys inside the "opcodes" object; nested fields (count,
            // category, ...) sit at 6+ spaces, so indentation disambiguates.
            let rest = line.strip_prefix("    \"")?;
            if line.starts_with("      ") || !line.trim_end().ends_with('{') {
                return None;
            }
            rest.split('"').next()
        })
        .filter(|name| !name.is_empty())
        .collect();

    let enum_names: BTreeSet<String> = Opcode::ALL.iter().map(|o| format!("{o:?}")).collect();
    let enum_names: BTreeSet<&str> = enum_names.iter().map(String::as_str).collect();

    assert_eq!(
        enum_names, harvested,
        "Opcode::ALL must list exactly tools/opcodes-v2.json's harvested opcode set"
    );
}
