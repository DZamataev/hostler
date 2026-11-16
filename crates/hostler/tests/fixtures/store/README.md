# Store fixtures

## `hostler-0.0.2.db`

The database a released hostler 0.0.2 leaves (schema version 2). `store::schema`'s
`v2_database_from_0_0_2_migrates_to_v4` copies it to a temporary directory and opens it, so the
migration from 0.0.2 straight to the current schema runs against a file the real release wrote,
not one built by hand.

Made once on macOS on 2026-11-08 with the published binary:

```sh
cargo install hostler --version 0.0.2 --root <scratch>/root --locked
mkdir -p /tmp/hostler-fixture/home /tmp/hostler-fixture/app
chmod 700 /tmp/hostler-fixture/home
cd /tmp/hostler-fixture/app
git init -q -b main . && git -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -q --allow-empty -m init
export HOSTLER_HOME=/tmp/hostler-fixture/home
<scratch>/root/bin/hostler project                         # records the project "app"
<scratch>/root/bin/hostler run --heavy -- echo heavy-ok    # starts the service; one history row
<scratch>/root/bin/hostler service stop                    # checkpoints the WAL into the file
cp /tmp/hostler-fixture/home/hostler.db tests/fixtures/store/hostler-0.0.2.db
sqlite3 tests/fixtures/store/hostler-0.0.2.db 'VACUUM'   # see below
rm -f tests/fixtures/store/hostler-0.0.2.db-*            # the macOS sqlite3 leaves them behind
```

The `VACUUM` rewrites every page, so the free pages no longer hold the deleted `heavy_entries` row
(the run's pids and the machine's boot id). It keeps `user_version` 2, the schema and WAL mode.
`v2_database_from_0_0_2_migrates_to_v4` checks that the file holds no `boot_id`, `"pid"` or
`/Users/` bytes. The `-wal` / `-shm` side files are ignored here (`.gitignore`): open the fixture
only through a copy, as the test does.

What it holds (`PRAGMA user_version` = 2, journal mode WAL, no `-wal` file needed, vacuumed):

- `projects`: one row, `app`, common dir `/private/tmp/hostler-fixture/app/.git`, not approved;
- `heavy_history`: one row, kind `other`, command `echo heavy-ok`, exit code 0;
- `settings`: `heavy.next_ticket` = `2`;
- every other table empty.

The paths are the neutral `/tmp/hostler-fixture` ones above; nothing else of the machine is in it.
