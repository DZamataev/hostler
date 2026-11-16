//! Where hostler keeps secrets inside the KDBX: one group per project (plan P2, Task 4).
//!
//! An entry's `Title` is the secret NAME (unprotected), its `Password` the value (protected).
//! A project group is found by UUID anywhere in the tree; one under the Recycle Bin is gone.
//!
//! An account (plan P3a, Task 9) is an entry tagged `hostler-account`: its `Title` is the account
//! name (unprotected), each field one protected custom string. `UserName`, `Password`, `URL` and
//! `Notes` are never read.

use keepass::Database;
use keepass::db::{EntryId, GroupId, fields};

use super::valid_secret_name;
use super::value::SecretValue;
use crate::contract::accounts::{valid_account_name, valid_field_name};

pub const ACCOUNT_TAG: &str = "hostler-account";

pub enum Lookup {
    Missing,
    Value(SecretValue),
    Duplicate,
}

pub enum AccountLookup {
    Missing,
    /// `(field, value)`, sorted by field.
    Found(Vec<(String, SecretValue)>),
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

fn is_account(e: &keepass::db::Entry) -> bool {
    e.tags.iter().any(|t| t == ACCOUNT_TAG)
}

/// Tagged entries of `group` (direct children only) whose title is `name`.
fn accounts_named(db: &Database, group: GroupId, name: &str) -> Vec<EntryId> {
    if !live_group(db, group) {
        return Vec::new();
    }
    db.group(group)
        .map(|g| {
            g.entries()
                .filter(|e| is_account(e) && e.get_title() == Some(name))
                .map(|e| e.id())
                .collect()
        })
        .unwrap_or_default()
}

/// An entry's account fields: custom strings with a valid field name; an empty one is unset.
fn field_entries(e: &keepass::db::Entry) -> impl Iterator<Item = (&String, &str)> {
    e.fields
        .iter()
        .map(|(k, v)| (k, v.get().as_str()))
        .filter(|(k, v)| !fields::KNOWN_FIELDS.contains(&k.as_str()) && !v.is_empty())
        .filter(|(k, _)| valid_field_name(k))
}

/// Titles of the group's tagged entries that are valid account names, sorted, deduplicated.
pub fn account_names(db: &Database, group: GroupId) -> Vec<String> {
    if !live_group(db, group) {
        return Vec::new();
    }
    let mut names: Vec<String> = db
        .group(group)
        .map(|g| {
            g.entries()
                .filter(|e| is_account(e))
                .filter_map(|e| e.get_title().map(str::to_string))
                .filter(|t| valid_account_name(t))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

/// The field names of account `name` (of every entry, when it is duplicated), sorted.
pub fn account_fields(db: &Database, group: GroupId, name: &str) -> Option<Vec<String>> {
    let ids = accounts_named(db, group, name);
    if ids.is_empty() {
        return None;
    }
    let mut names: Vec<String> = ids
        .iter()
        .filter_map(|id| db.entry(*id))
        .flat_map(|e| {
            field_entries(&e)
                .map(|(field, _)| field.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    names.sort();
    names.dedup();
    Some(names)
}

pub fn account(db: &Database, group: GroupId, name: &str) -> AccountLookup {
    match accounts_named(db, group, name).as_slice() {
        [] => AccountLookup::Missing,
        [id] => db.entry(*id).map_or(AccountLookup::Missing, |e| {
            let mut fields: Vec<(String, SecretValue)> = field_entries(&e)
                .map(|(k, v)| (k.clone(), SecretValue::new(v.to_owned())))
                .collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            AccountLookup::Found(fields)
        }),
        _ => AccountLookup::Duplicate,
    }
}

/// Sets `field` of account `name` in `group` (which must exist), adding the tagged entry when it
/// is new. The caller refuses duplicates before.
pub fn put_account_field(
    db: &mut Database,
    group: GroupId,
    name: &str,
    field: &str,
    value: &SecretValue,
) {
    if let Some(id) = accounts_named(db, group, name).first() {
        if let Some(mut entry) = db.entry_mut(*id) {
            entry.set_protected(field, value.expose());
        }
        return;
    }
    let mut group = db
        .group_mut(group)
        .expect("layout::put_account_field into a group that does not exist");
    let mut entry = group.add_entry();
    entry.set_unprotected(fields::TITLE, name);
    entry.tags.push(ACCOUNT_TAG.to_string());
    entry.set_protected(field, value.expose());
}

/// Removes hostler's data of account `name` (every tagged entry of that title): only `field`, or
/// every account field. An entry left without account fields loses the tag too (L26); it is
/// deleted (recorded in `deleted_objects`) only when nothing else has content — a standard
/// field, another custom string or an attachment keeps it, as a plain entry.
pub fn remove_account(db: &mut Database, group: GroupId, name: &str, field: Option<&str>) -> bool {
    let mut removed = false;
    for id in accounts_named(db, group, name) {
        let Some(mut entry) = db.entry_mut(id) else {
            continue;
        };
        let ours: Vec<String> = match field {
            Some(field) if entry.fields.contains_key(field) => vec![field.to_string()],
            Some(_) => continue,
            None => field_entries(&entry).map(|(k, _)| k.clone()).collect(),
        };
        for field in &ours {
            entry.fields.remove(field);
        }
        removed = true;
        if field_entries(&entry).next().is_some() {
            continue;
        }
        entry.tags.retain(|t| t != ACCOUNT_TAG);
        let other_data = entry
            .fields
            .iter()
            .any(|(k, v)| k != fields::TITLE && !v.get().is_empty())
            || entry.as_ref().attachments().next().is_some();
        if !other_data {
            entry.track_changes().remove();
        }
    }
    removed
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

    fn fields(l: AccountLookup) -> Vec<(String, String)> {
        match l {
            AccountLookup::Found(f) => f
                .into_iter()
                .map(|(k, v)| (k, v.expose().to_string()))
                .collect(),
            AccountLookup::Missing => panic!("missing"),
            AccountLookup::Duplicate => panic!("duplicate"),
        }
    }

    #[test]
    fn put_field_creates_tagged_entry() {
        let (mut db, g) = db_with_group();
        assert!(matches!(account(&db, g, "qa-user"), AccountLookup::Missing));
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        put_account_field(&mut db, g, "qa-user", "env", &value("stage"));
        let entries: Vec<_> = db.group(g).unwrap().entries().map(|e| e.id()).collect();
        assert_eq!(entries.len(), 1, "the second field joins the first entry");
        let e = db.entry(entries[0]).unwrap();
        assert_eq!(e.tags, vec![ACCOUNT_TAG.to_string()]);
        assert_eq!(e.get_title(), Some("qa-user"));
        assert!(e.get(fields::PASSWORD).is_none());
        assert_eq!(
            fields(account(&db, g, "qa-user")),
            vec![
                ("env".to_string(), "stage".to_string()),
                ("session".to_string(), "sess-1234".to_string())
            ]
        );
        put_account_field(&mut db, g, "qa-user", "env", &value("prod-1"));
        assert_eq!(fields(account(&db, g, "qa-user"))[0].1, "prod-1");
        assert_eq!(account_names(&db, g), vec!["qa-user".to_string()]);
        assert_eq!(
            account_fields(&db, g, "qa-user"),
            Some(vec!["env".to_string(), "session".to_string()])
        );
        assert_eq!(account_fields(&db, g, "other"), None);
    }

    #[test]
    fn fields_are_protected_custom_strings() {
        let (mut db, g) = db_with_group();
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        let id = db.group(g).unwrap().entries().next().unwrap().id();
        let e = db.entry(id).unwrap();
        assert!(e.fields["session"].is_protected());
        assert!(!e.fields[fields::TITLE].is_protected());
        // Standard fields, invalid names and empty values are not account fields.
        let mut e = db.entry_mut(id).unwrap();
        e.set_protected(fields::PASSWORD, "pw-value");
        e.set_unprotected(fields::NOTES, "notes");
        e.set_protected("KP2A_URL", "x-value");
        e.set_protected("blank", "");
        assert_eq!(
            account_fields(&db, g, "qa-user"),
            Some(vec!["session".to_string()])
        );
    }

    #[test]
    fn untagged_entry_is_not_an_account() {
        let (mut db, g) = db_with_group();
        {
            let mut group = db.group_mut(g).unwrap();
            let mut e = group.add_entry();
            e.set_unprotected(fields::TITLE, "qa-user");
            e.set_protected("session", "sess-1234");
        }
        assert!(account_names(&db, g).is_empty());
        assert!(matches!(account(&db, g, "qa-user"), AccountLookup::Missing));
        assert!(!remove_account(&mut db, g, "qa-user", None));
        // A tagged entry with an invalid title is not listed either.
        put_account_field(&mut db, g, "Bad Name", "session", &value("sess-1234"));
        assert!(account_names(&db, g).is_empty());
    }

    #[test]
    fn secret_names_never_list_accounts() {
        let (mut db, g) = db_with_group();
        put(&mut db, g, "API_KEY", &value("v1-value"));
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        assert_eq!(names(&db, g), vec!["API_KEY".to_string()]);
        assert!(matches!(lookup(&db, g, "qa-user"), Lookup::Missing));
        assert_eq!(account_names(&db, g), vec!["qa-user".to_string()]);
        assert!(account_fields(&db, g, "API_KEY").is_none());
    }

    #[test]
    fn remove_field_then_account() {
        let (mut db, g) = db_with_group();
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        put_account_field(&mut db, g, "qa-user", "env", &value("stage"));
        let id = db.group(g).unwrap().entries().next().unwrap().id();
        assert!(!remove_account(&mut db, g, "qa-user", Some("nope")));
        assert!(remove_account(&mut db, g, "qa-user", Some("env")));
        assert_eq!(
            account_fields(&db, g, "qa-user"),
            Some(vec!["session".to_string()])
        );
        // Removing the last field removes the account.
        assert!(remove_account(&mut db, g, "qa-user", Some("session")));
        assert!(db.entry(id).is_none());
        assert!(db.deleted_objects.contains_key(&id.uuid()));
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        assert!(remove_account(&mut db, g, "qa-user", None));
        assert!(matches!(account(&db, g, "qa-user"), AccountLookup::Missing));
        assert!(!remove_account(&mut db, g, "qa-user", None));
    }

    #[test]
    fn duplicate_tagged_entries_are_reported() {
        let (mut db, g) = db_with_group();
        for v in ["first-value", "second-value"] {
            let mut group = db.group_mut(g).unwrap();
            let mut e = group.add_entry();
            e.set_unprotected(fields::TITLE, "qa-user");
            e.tags.push(ACCOUNT_TAG.into());
            e.set_protected("session", v);
        }
        assert!(matches!(
            account(&db, g, "qa-user"),
            AccountLookup::Duplicate
        ));
        assert_eq!(account_names(&db, g), vec!["qa-user".to_string()]);
        assert!(remove_account(&mut db, g, "qa-user", None));
        assert!(db.group(g).unwrap().entries().next().is_none());
    }

    /// L26: a person's own data on the entry survives `remove --field` and `remove NAME`.
    #[test]
    fn remove_keeps_the_persons_own_fields() {
        for field in [Some("session"), None] {
            let (mut db, g) = db_with_group();
            put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
            let id = db.group(g).unwrap().entries().next().unwrap().id();
            {
                let mut e = db.entry_mut(id).unwrap();
                e.set_protected(fields::PASSWORD, "login-pw-1");
                e.set_unprotected(fields::NOTES, "my notes");
                e.tags.push("work".into());
            }
            assert!(remove_account(&mut db, g, "qa-user", field));
            let e = db.entry(id).expect("the entry stays");
            assert_eq!(e.get(fields::PASSWORD), Some("login-pw-1"));
            assert_eq!(e.get(fields::NOTES), Some("my notes"));
            assert_eq!(e.get_title(), Some("qa-user"));
            assert!(e.get("session").is_none());
            assert_eq!(e.tags, vec!["work".to_string()]);
            assert!(!db.deleted_objects.contains_key(&id.uuid()));
            assert!(matches!(account(&db, g, "qa-user"), AccountLookup::Missing));
            assert!(account_names(&db, g).is_empty());
            assert!(!remove_account(&mut db, g, "qa-user", None));
        }
        // An entry with only hostler's data is still deleted.
        let (mut db, g) = db_with_group();
        put_account_field(&mut db, g, "qa-user", "session", &value("sess-1234"));
        put_account_field(&mut db, g, "qa-user", "env", &value("stage"));
        let id = db.group(g).unwrap().entries().next().unwrap().id();
        assert!(remove_account(&mut db, g, "qa-user", None));
        assert!(db.entry(id).is_none());
        assert!(db.deleted_objects.contains_key(&id.uuid()));
    }
}
