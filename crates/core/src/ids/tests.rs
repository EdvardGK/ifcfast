#[cfg(test)]
mod restriction_tests {
    use crate::ids::compiled::ValueConstraint;
    use crate::ids::entity_schema::{entity_is_subtype_or_equal, entity_matches_names};
    use crate::ids::restrictions::{ids_datatype_matches, value_matches};

    #[test]
    fn simple_value_equality() {
        let c = ValueConstraint::Simple {
            text: "foo".into(),
        };
        assert!(value_matches(Some("foo"), Some(&c)));
        assert!(!value_matches(Some("bar"), Some(&c)));
    }

    #[test]
    fn enumeration_match() {
        let c = ValueConstraint::Enumeration {
            values: vec!["A".into(), "B".into()],
        };
        assert!(value_matches(Some("A"), Some(&c)));
        assert!(!value_matches(Some("C"), Some(&c)));
    }

    #[test]
    fn pattern_fullmatch_not_substring() {
        let c = ValueConstraint::Pattern {
            patterns: vec![r"Wall.*".into()],
        };
        assert!(value_matches(Some("Wall-001"), Some(&c)));
        assert!(!value_matches(Some("My-Wall-001"), Some(&c)));
    }

    #[test]
    fn bounds_inclusive() {
        let c = ValueConstraint::Bounds {
            min_inclusive: Some(1.0),
            max_inclusive: Some(10.0),
            min_exclusive: None,
            max_exclusive: None,
        };
        assert!(value_matches(Some("5"), Some(&c)));
        assert!(!value_matches(Some("0.5"), Some(&c)));
        assert!(value_matches(Some("1"), Some(&c)));
        assert!(value_matches(Some("10"), Some(&c)));
    }

    #[test]
    fn bounds_exclusive() {
        let c = ValueConstraint::Bounds {
            min_inclusive: None,
            max_inclusive: None,
            min_exclusive: Some(0.0),
            max_exclusive: Some(10.0),
        };
        assert!(value_matches(Some("5"), Some(&c)));
        assert!(!value_matches(Some("0"), Some(&c)));
        assert!(!value_matches(Some("10"), Some(&c)));
    }

    #[test]
    fn pattern_all_must_match_ifctester() {
        let c = ValueConstraint::Pattern {
            patterns: vec![r"[A-Z]{2}[0-9]{2}".into(), r"[a-z]{2}[0-9]{2}".into()],
        };
        assert!(!value_matches(Some("AB12"), Some(&c)));
        assert!(!value_matches(Some("ab12"), Some(&c)));
        assert!(!value_matches(Some("A1"), Some(&c)));
    }

    #[test]
    fn datatype_ifc_prefix() {
        assert!(ids_datatype_matches(
            Some("IfcLabel"),
            "IFCLABEL"
        ));
    }

    #[test]
    fn entity_subtype_wall_is_building_element() {
        assert!(entity_is_subtype_or_equal(
            "IFCWALL",
            "IFCBUILDINGELEMENT"
        ));
        assert!(!entity_matches_names(
            "IFCWALLSTANDARDCASE",
            &["IFCWALL".into()],
            false
        ));
        assert!(entity_matches_names(
            "IFCWALLSTANDARDCASE",
            &["IFCWALL".into()],
            true
        ));
    }
}
