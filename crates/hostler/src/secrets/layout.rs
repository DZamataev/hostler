//! Where hostler keeps secrets inside the KDBX: one group per project (plan P2, Task 4).
//!
//! An entry's `Title` is the secret NAME (unprotected), its `Password` the value (protected).
//! A project group is found by UUID anywhere in the tree; one under the Recycle Bin is gone.

use keepass::Database;
use keepass::db::{EntryId, GroupId, fields};

use super::valid_secret_name;
use super::value::SecretValue;

pub enum Lookup {
    Missing,
    Value(SecretValue),
    Duplicate,
}

/// The group exists and neither it nor any ancestor is the Recycle Bin.
pub fn live_group(db: &Database, id: GroupId) -> bool {
    let bin = db.meta.recyclebin_uuid.map(GroupId::from_uuid);
    if db.group(id).is_none() {
        return false;
    }
    let mut current = Some(id);
    while let Some(c) = current {
        if Some(c) == bin {
            return false;
        }
        current = db.group(c).and_then(|g| g.parent().map(|p| p.id()));
    }
    true
}

/// The live group `id`, or a new group named `name` under the root (a fresh UUID: an id in the
/// Recycle Bin cannot be reused).
pub fn ensure_group(db: &mut Database, id: Option<GroupId>, name: &str) -> GroupId {
    if let Some(id) = id
        && live_group(db, id)
    {
        return id;
    }
    let mut root = db.root_mut();
    let mut group = root.add_group();
    group.name = name.to_string();
    group.id()
}

/// Entry ids of `group` (direct children only) whose title is `name`.
fn entries_named(db: &Database, group: GroupId, name: &str) -> Vec<EntryId> {
    if !live_group(db, group) {
        return Vec::new();
    }
    db.group(group)
        .map(|g| {
            g.entries()
                .filter(|e| e.get_title() == Some(name))
                .map(|e| e.id())
                .collect()
        })
        .unwrap_or_default()
}

/// Titles of the group's entries that are valid secret names, sorted, deduplicated.
pub fn names(db: &Database, group: GroupId) -> Vec<String> {
    if !live_group(db, group) {
        return Vec::new();
    }
    let mut names: Vec<String> = db
        .group(group)
        .map(|g| {
            g.entries()
                .filter_map(|e| e.get_title().map(str::to_string))
                .filter(|t| valid_secret_name(t))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

/// An entry with an empty `Password` counts as missing; two entries of one title are a duplicate.
pub fn lookup(db: &Database, group: GroupId, name: &str) -> Lookup {
    let ids = entries_named(db, group, name);
    match ids.as_slice() {
        [] => Lookup::Missing,
        [id] => match db
            .entry(*id)
            .and_then(|e| e.get(fields::PASSWORD).map(str::to_string))
        {
            Some(v) if !v.is_empty() => Lookup::Value(SecretValue::new(v)),
            _ => Lookup::Missing,
        },
        _ => Lookup::Duplicate,
    }
}

/// Sets `name` to `value` in `group` (which must exist): updates the entry in place (same id, no
/// history entry) or adds one. The caller refuses duplicates before.
pub fn put(db: &mut Database, group: GroupId, name: &str, value: &SecretValue) {
    if let Some(id) = entries_named(db, group, name).first() {
        if let Some(mut entry) = db.entry_mut(*id) {
            entry.set_protected(fields::PASSWORD, value.expose());
        }
        return;
    }
    let mut group = db
        .group_mut(group)
        .expect("layout::put into a group that does not exist");
    let mut entry = group.add_entry();
    entry.set_unprotected(fields::TITLE, name);
    entry.set_protected(fields::PASSWORD, value.expose());
}

/// Removes every entry titled `name` from `group`, recording them in `deleted_objects`.
pub fn remove(db: &mut Database, group: GroupId, name: &str) -> bool {
    let ids = entries_named(db, group, name);
    for id in &ids {
        if let Some(mut entry) = db.entry_mut(*id) {
            entry.track_changes().remove();
        }
    }
    !ids.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(s: &str) -> SecretValue {
        SecretValue::new(s.into())
    }

    fn expect_value(l: Lookup) -> String {
        match l {
            Lookup::Value(v) => v.expose().to_string(),
            Lookup::Missing => panic!("missing"),
            Lookup::Duplicate => panic!("duplicate"),
        }
    }

    fn db_with_group() -> (Database, GroupId) {
        let mut db = Database::new();
        let g = ensure_group(&mut db, None, "proj");
        (db, g)
    }

    #[test]
    fn put_then_lookup() {
        let (mut db, g) = db_with_group();
        assert!(matches!(lookup(&db, g, "API_KEY"), Lookup::Missing));
        put(&mut db, g, "API_KEY", &value("v1-value"));
        assert!(expect_value(lookup(&db, g, "API_KEY")) == "v1-value");
        let e = db.group(g).unwrap().entries().next().unwrap().id();
        assert!(db.entry(e).unwrap().fields[fields::PASSWORD].is_protected());
        assert!(!db.entry(e).unwrap().fields[fields::TITLE].is_protected());
        assert_eq!(names(&db, g), vec!["API_KEY".to_string()]);
    }

    #[test]
    fn put_updates_in_place() {
        let (mut db, g) = db_with_group();
        put(&mut db, g, "API_KEY", &value("v1-value"));
        let before: Vec<EntryId> = db.group(g).unwrap().entries().map(|e| e.id()).collect();
        put(&mut db, g, "API_KEY", &value("v2-value"));
        let after: Vec<EntryId> = db.group(g).unwrap().entries().map(|e| e.id()).collect();
        assert_eq!(before, after);
        assert_eq!(after.len(), 1);
        assert!(expect_value(lookup(&db, g, "API_KEY")) == "v2-value");
        let history = db.entry(after[0]).unwrap().history.clone();
        assert!(history.is_none_or(|h| h.get_entries().is_empty()));
    }

    #[test]
    fn remove_records_deleted_object() {
        let (mut db, g) = db_with_group();
        put(&mut db, g, "API_KEY", &value("v1-value"));
        let id = db.group(g).unwrap().entries().next().unwrap().id();
        assert!(remove(&mut db, g, "API_KEY"));
        assert!(db.entry(id).is_none());
        assert!(db.deleted_objects.contains_key(&id.uuid()));
        assert!(matches!(lookup(&db, g, "API_KEY"), Lookup::Missing));
        assert!(!remove(&mut db, g, "API_KEY"));
    }

    #[test]
    fn duplicate_titles_are_reported() {
        let (mut db, g) = db_with_group();
        for v in ["first-value", "second-value"] {
            let mut group = db.group_mut(g).unwrap();
            let mut e = group.add_entry();
            e.set_unprotected(fields::TITLE, "TOKEN");
            e.set_protected(fields::PASSWORD, v);
        }
        assert!(matches!(lookup(&db, g, "TOKEN"), Lookup::Duplicate));
        assert_eq!(names(&db, g), vec!["TOKEN".to_string()]);
    }

    #[test]
    fn empty_password_counts_as_missing() {
        let (mut db, g) = db_with_group();
        let mut group = db.group_mut(g).unwrap();
        group.add_entry().set_unprotected(fields::TITLE, "EMPTY");
        assert!(matches!(lookup(&db, g, "EMPTY"), Lookup::Missing));
    }

    #[test]
    fn names_skip_invalid_titles() {
        let (mut db, g) = db_with_group();
        for t in ["B_KEY", "lower", "A_KEY", "has space", "B_KEY"] {
            let mut group = db.group_mut(g).unwrap();
            let mut e = group.add_entry();
            e.set_unprotected(fields::TITLE, t);
            e.set_protected(fields::PASSWORD, "some-value");
        }
        // An untitled entry is skipped too.
        db.group_mut(g).unwrap().add_entry();
        assert_eq!(
            names(&db, g),
            vec!["A_KEY".to_string(), "B_KEY".to_string()]
        );
    }

    #[test]
    fn group_in_recycle_bin_is_not_live() {
        let (mut db, g) = db_with_group();
        put(&mut db, g, "API_KEY", &value("v1-value"));
        // KeePassXC deletes a group by moving it into the Recycle Bin.
        let bin = {
            let mut root = db.root_mut();
            let mut bin = root.add_group();
            bin.name = "Recycle Bin".into();
            bin.id()
        };
        db.meta.recyclebin_uuid = Some(bin.uuid());
        db.group_mut(g).unwrap().move_to(bin).unwrap();
        assert!(!live_group(&db, g));
        assert!(names(&db, g).is_empty());
        assert!(matches!(lookup(&db, g, "API_KEY"), Lookup::Missing));
        let fresh = ensure_group(&mut db, Some(g), "proj");
        assert_ne!(fresh, g);
        assert!(live_group(&db, fresh));
        assert_eq!(db.group(fresh).unwrap().name, "proj");
        assert!(!live_group(&db, GroupId::new()));
    }

    #[test]
    fn group_moved_into_subgroup_is_still_found() {
        let (mut db, g) = db_with_group();
        put(&mut db, g, "API_KEY", &value("v1-value"));
        let parent = {
            let mut root = db.root_mut();
            let mut p = root.add_group();
            p.name = "work".into();
            p.id()
        };
        db.group_mut(g).unwrap().move_to(parent).unwrap();
        assert!(live_group(&db, g));
        assert_eq!(ensure_group(&mut db, Some(g), "proj"), g);
        assert!(expect_value(lookup(&db, g, "API_KEY")) == "v1-value");
    }
}
