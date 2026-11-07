# P2 KDBX fixtures (made 2026-11-07 with KeePassXC 2.7.10 `keepassxc-cli` and keepass 0.17.0)

Password of every file: `fixture-pass-1`. Values are test-only.

| file | format as written | KDF | contents |
|---|---|---|---|
| kxc-3.1-aes.kdbx | KDBX 3.1 (keepassxc-cli db-create default) | AES-KDF, ~13M rounds (`-t 100`) | group `fixture`, entry `TOKEN` = `fixture-token-value` |
| kxc-4.0-argon2id.kdbx | KDBX 4.0 (written by keepass 0.17 as 4.1, then re-saved by keepassxc-cli `add`, which writes 4.0) | Argon2id 2 it, 1 MiB, p1 | group `proj-a`: `API_KEY` = `s3cr3t-value` (keepass-written), `TOKEN` = `fixture-token-value` (KeePassXC-written) |
| kxc-keyfile.kdbx + kxc-keyfile.keyx | KDBX 3.1, password + XML v2 key file | AES-KDF ~13M rounds | group `fixture`, entry `TOKEN` = `fixture-token-value` |

Recreate:
  printf 'fixture-pass-1\nfixture-pass-1\n' | keepassxc-cli db-create -q -p -t 100 kxc-3.1-aes.kdbx
  printf 'fixture-pass-1\n' | keepassxc-cli mkdir -q kxc-3.1-aes.kdbx fixture
  printf 'fixture-pass-1\nfixture-token-value\n' | keepassxc-cli add -q -p kxc-3.1-aes.kdbx fixture/TOKEN
  (key file: add `--set-key-file kxc-keyfile.keyx` to db-create and `-k kxc-keyfile.keyx` to mkdir/add)
