//! Permission-node matching, with the same rules as Vine's
//! `upstream/Vine/src/permissions.rs`, so a rank's nodes mean the same thing
//! whether Vine, the control plane or a plugin checks them. Shared by
//! `hallow-db` (ranks) and the Vine plugin (staff commands).

/// Whether `pattern` grants `node`: `*` matches everything, `a.b.*` matches
/// anything below `a.b` (not `a.b` itself, and not `a.bc`), and otherwise
/// the two must be equal. Case is ignored.
pub fn matches_node(pattern: &str, node: &str) -> bool {
    if pattern == "*" || pattern.eq_ignore_ascii_case(node) {
        return true;
    }
    match pattern.strip_suffix(".*") {
        // A `.` byte always starts a character, so the slice can't split one.
        Some(prefix) => {
            node.len() > prefix.len()
                && node.as_bytes()[prefix.len()] == b'.'
                && node[..prefix.len()].eq_ignore_ascii_case(prefix)
        }
        None => false,
    }
}

/// Whether `nodes` grant `node`. A denial (`-node`, matched the same way)
/// beats any grant, as in Vine.
pub fn has_permission<S: AsRef<str>>(nodes: &[S], node: &str) -> bool {
    let nodes = || nodes.iter().map(AsRef::as_ref);
    if nodes()
        .filter_map(|n| n.strip_prefix('-'))
        .any(|denied| matches_node(denied, node))
    {
        return false;
    }
    nodes()
        .filter(|n| !n.starts_with('-'))
        .any(|granted| matches_node(granted, node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_follows_vine() {
        assert!(matches_node("*", "hallow.staff.find"));
        assert!(matches_node("hallow.staff.find", "hallow.staff.find"));
        assert!(matches_node("Hallow.Staff.Find", "hallow.staff.find"));
        assert!(matches_node("hallow.staff.*", "hallow.staff.find"));
        assert!(matches_node("hallow.staff.*", "HALLOW.STAFF.find"));
        assert!(matches_node("hallow.*", "hallow.staff.find"));
        assert!(!matches_node("hallow.staff.*", "hallow.staff"));
        assert!(!matches_node("hallow.staff.*", "hallow.staffer.find"));
        assert!(!matches_node("hallow.staff.find", "hallow.staff"));
    }

    #[test]
    fn denials_win() {
        let nodes = ["hallow.staff.*", "-hallow.staff.ban"];
        assert!(has_permission(&nodes, "hallow.staff.find"));
        assert!(!has_permission(&nodes, "hallow.staff.ban"));
        assert!(!has_permission(
            &["*", "-hallow.staff.*"],
            "hallow.staff.find"
        ));
        assert!(!has_permission::<&str>(&[], "hallow.staff.find"));
    }
}
