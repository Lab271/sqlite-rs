//! Confirms the `mvl::ensures`/`mvl::requires` `assert!` injections
//! actually fire correctly at runtime, independent of what `cargo mvl
//! prove` reports about their static-proof `layer`. See `../findings.md`.

use spike_mvl_refinements::{
    cast_on_bare_param_also_blocked, compute_usable_page_size, known_shape_fold_in_isolation,
    known_shape_fold_not_propagated_through_or, DatabaseHeader, Page, PageWithNarrowField,
};

#[test]
fn parse_accepts_a_well_formed_header() {
    let buf = [0u8; 100];
    assert!(DatabaseHeader::parse(&buf).is_ok());
}

#[test]
fn parse_rejects_a_too_short_buffer_without_panicking() {
    let buf = [0u8; 10];
    assert!(DatabaseHeader::parse(&buf).is_err());
}

#[test]
fn usable_page_size_matches_by_hand_arithmetic() {
    let header = DatabaseHeader {
        page_size: 4096,
        reserved_space: 12,
    };
    assert_eq!(header.usable_page_size(), 4084);
}

#[test]
fn compute_usable_page_size_matches_by_hand_arithmetic() {
    assert_eq!(compute_usable_page_size(4096, 12), 4084);
}

#[test]
fn field_projection_helper_matches_by_hand_arithmetic() {
    let page = Page {
        page_size: 4096,
        reserved_space: 12,
    };
    assert_eq!(page.usable_page_size_field_projection(), 4084);
}

#[test]
fn cast_helper_matches_by_hand_arithmetic() {
    let page = PageWithNarrowField {
        page_size: 4096,
        reserved_space: 12,
    };
    assert_eq!(page.usable_page_size_with_cast(), 4084);
    assert_eq!(cast_on_bare_param_also_blocked(4096, 12), 4084);
}

#[test]
fn known_shape_fold_functions_run_without_tripping_their_own_assertions() {
    assert!(known_shape_fold_in_isolation(-1).is_err());
    assert!(known_shape_fold_in_isolation(1).is_err());
    assert!(known_shape_fold_not_propagated_through_or(-1).is_err());
    assert_eq!(known_shape_fold_not_propagated_through_or(5).unwrap(), 5);
}
