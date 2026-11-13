#!/usr/bin/env bash
# End-to-end test against a real, disposable Postgres. Needs psql.
# Usage: PGIT_TEST_URL=postgres://postgres:password@localhost:54329 tests/e2e.sh [path/to/pgit]
set -euo pipefail

URL="${PGIT_TEST_URL:?set PGIT_TEST_URL to a throwaway server (databases get created and dropped)}"
B="$(cd "$(dirname "${1:-target/debug/pgit}")" && pwd)/$(basename "${1:-target/debug/pgit}")"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
cd "$T"

q() { psql "$URL/${2:-postgres}" -tAqc "$1"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
eq() { [[ "$1" == "$2" ]] || fail "expected '$2', got '$1'"; }
must_fail() { if "$@" >/dev/null 2>&1; then fail "should have failed: $*"; fi; }

# Clean slate on the server
for db in $(q "SELECT datname FROM pg_database WHERE datname LIKE 'db_branch_%' OR datname = 'app'"); do
  q "DROP DATABASE \"$db\" WITH (FORCE)"
done
q "CREATE DATABASE app"
q "ALTER DATABASE app SET search_path TO ti, etl, public"
q "ALTER DATABASE app SET timezone TO 'Europe/Paris'"
q "ALTER ROLE CURRENT_USER IN DATABASE app SET work_mem TO '8MB'"
q "CREATE SCHEMA ti; CREATE TABLE ti.t(x int); INSERT INTO ti.t VALUES (42)" app

git init -q -b main && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
REPO="$(git rev-parse --path-format=absolute --git-common-dir)"
touch mise.toml
"$B" init
sed -i.bak "s/template: postgres/template: app/; s|# - url: DATABASE_URL|- url: OTHER_DB|; s|# connection:|connection:|; s|#   url: .*|  url: $URL|" pgit.yaml

# create: data, db and role settings, marker, env file, status
git checkout -q -b feature/TI-1-x
"$B" create
DB=db_branch_feature_ti_1_x
eq "$(q 'SELECT x FROM t' $DB)" 42
eq "$(q 'SHOW search_path' $DB)" "ti, etl, public"
eq "$(q 'SHOW timezone' $DB)" "Europe/Paris"
eq "$(q 'SHOW work_mem' $DB)" "8MB"
eq "$(q "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = '$DB'")" "pgit:$REPO:feature/TI-1-x"
grep -q "DB_NAME = \"$DB\"" mise.local.toml || fail "env file not written"
grep -qF "OTHER_DB = \"$URL/$DB\"" mise.local.toml || fail "url key not written"
[[ "$("$B" status)" == *"database:  $DB (exists)"* ]] || fail "status should show the branch DB"
must_fail "$B" create

# guard on active template connections
psql "$URL/app" -c "SELECT pg_sleep(20)" >/dev/null 2>&1 &
sleep 1
must_fail "$B" create other
"$B" create other --force
wait || true

# --from, use, reset
"$B" create copy --from feature/TI-1-x
eq "$(q 'SELECT x FROM t' db_branch_copy)" 42
"$B" use
grep -q "DB_NAME = \"$DB\"" mise.local.toml || fail "use did not switch"
"$B" reset
[[ ! -e mise.local.toml ]] || fail "reset should remove the emptied env file"

# a value the user put in the env file is never overwritten or removed, and a refused create leaves no DB
printf '[env]\nDB_NAME = "my_own_db"\n' > mise.local.toml
cp mise.local.toml own.bak
must_fail "$B" create mine
eq "$(q "SELECT count(*) FROM pg_database WHERE datname = 'db_branch_mine'")" 0
must_fail "$B" use copy
must_fail "$B" reset
cmp -s mise.local.toml own.bak || fail "the user's env value was touched"
rm mise.local.toml own.bak

# drop safety: legacy DBs are listed, confirmation required without a TTY, open connections don't block it
q "CREATE DATABASE db_branch_legacy"
[[ "$("$B" list)" =~ db_branch_legacy\ +[0-9]+\ [a-zA-Z]+\ +\(legacy\) ]] || fail "legacy not listed"
# the template is never listed or dropped, even when it matches the name template
q "CREATE DATABASE db_branch_base"
sed -i.bak 's/template: app/template: db_branch_base/' pgit.yaml
[[ "$("$B" list)" != *db_branch_base* ]] || fail "template listed"
must_fail "$B" drop base -y
eq "$(q "SELECT count(*) FROM pg_database WHERE datname = 'db_branch_base'")" 1
# switching onto the template (its name fits the template) must not lock reset/use out
"$B" use base
"$B" reset
[[ ! -e mise.local.toml ]] || fail "reset after using the template"
sed -i.bak 's/template: db_branch_base/template: app/' pgit.yaml
q "DROP DATABASE db_branch_base"
must_fail "$B" drop copy </dev/null
"$B" use copy
psql "$URL/db_branch_copy" -c "SELECT pg_sleep(20)" >/dev/null 2>&1 &
sleep 1
"$B" drop copy -y
wait || true
[[ ! -e mise.local.toml ]] || fail "dropping the active DB should clear the override"

# prune drops DBs whose branch is gone (not with --dry-run), never legacy ones or another repo's;
# drop refuses another (still existing) repo's DB but works on legacy ones;
# a DB whose repo folder is gone (moved/deleted) counts as legacy
git init -q other-repo
q "CREATE DATABASE db_branch_foreign"
q "COMMENT ON DATABASE db_branch_foreign IS 'pgit:$(git -C other-repo rev-parse --path-format=absolute --git-common-dir):gone'"
[[ "$("$B" list)" != *db_branch_foreign* ]] || fail "another repo's DB listed"
must_fail "$B" drop foreign -y
q "CREATE DATABASE db_branch_moved"
q "COMMENT ON DATABASE db_branch_moved IS 'pgit:$T/moved-away/.git:gone'"
[[ "$("$B" list)" =~ db_branch_moved\ +[0-9]+\ [a-zA-Z]+\ +\(legacy\) ]] || fail "moved repo's DB not legacy"
# a repo path whose parent folder is missing too (container, other machine, unmounted volume) isn't gone
q "CREATE DATABASE db_branch_remote"
q "COMMENT ON DATABASE db_branch_remote IS 'pgit:$T/no-such-dir/app/.git:remote'"
[[ "$("$B" list)" != *db_branch_remote* ]] || fail "unreachable repo's DB listed"
must_fail "$B" drop remote -y
eq "$(q "SELECT count(*) FROM pg_database WHERE datname = 'db_branch_remote'")" 1
q "DROP DATABASE db_branch_remote"
[[ "$("$B" prune -y --dry-run)" == *db_branch_other* ]] || fail "dry-run should list the stale DB"
eq "$(q "SELECT count(*) FROM pg_database WHERE datname = 'db_branch_other'")" 1
"$B" prune -y
eq "$(q "SELECT count(*) FROM pg_database WHERE datname IN ('db_branch_other', 'db_branch_legacy', 'db_branch_foreign', 'db_branch_moved', '$DB')")" 4
"$B" drop legacy -y
"$B" drop moved -y

# with extract, a DB shared by two branches survives while one of them exists,
# and $DB (now mapped to another name) survives while its own branch exists
echo "extract: '([A-Z]+-\d+)'" >> pgit.yaml
git checkout -q -b feature/TI-9-a && "$B" create
git checkout -q -b fix/TI-9-b && git branch -q -D feature/TI-9-a
"$B" prune -y
eq "$(q "SELECT count(*) FROM pg_database WHERE datname IN ('db_branch_ti_9', '$DB')")" 2
"$B" drop -y && git checkout -q main
sed -i.bak '/^extract:/d' pgit.yaml

# hook: install keeps an existing hook, use mode switches/resets, create mode creates, a broken config never blocks checkout
"$B" hook install
must_fail "$B" hook install
export PATH="$(dirname "$B"):$PATH" # the installed hook calls `pgit` from PATH
git checkout -q main
[[ ! -e mise.local.toml ]] || fail "no branch DB for main, should stay on default"
git checkout -q feature/TI-1-x
grep -q "DB_NAME = \"$DB\"" mise.local.toml || fail "hook did not switch"
git checkout -q main
[[ ! -e mise.local.toml ]] || fail "hook did not reset"
sed -i.bak 's/hook: use/hook: create/' pgit.yaml
git checkout -q -b feat/new
eq "$(q "SELECT count(*) FROM pg_database WHERE datname = 'db_branch_feat_new'")" 1

# config in .config/ is found from a subdir, env file still lands in the project root
mkdir -p .config sub && mv pgit.yaml .config/pgit.yaml && rm -f mise.local.toml
(cd sub && "$B" use)
grep -q 'DB_NAME = "db_branch_feat_new"' mise.local.toml || fail ".config/pgit.yaml: env file not in root"
[[ ! -e .config/mise.local.toml ]] || fail "env file written next to .config config"
cp .config/pgit.yaml .config/.pgit.yaml
must_fail "$B" use
rm .config/.pgit.yaml && mv .config/pgit.yaml pgit.yaml

# connection.fnox: url comes from `fnox get <key>` (fake fnox on PATH)
mkdir -p fakebin
printf '#!/bin/sh\n[ "$1 $2" = "get PG_URL" ] && echo "%s"\n' "$URL" > fakebin/fnox && chmod +x fakebin/fnox
sed -i.bak "s|  url: .*|  fnox: PG_URL|" pgit.yaml
[[ "$(PATH="$PWD/fakebin:$PATH" "$B" list)" == *db_branch_feat_new* ]] || fail "fnox url not used"
sed -i.bak "s|  fnox: PG_URL|  fnox: MISSING|" pgit.yaml
PATH="$PWD/fakebin:$PATH" must_fail "$B" list
sed -i.bak "s|  fnox: MISSING|  url: $URL|" pgit.yaml

# a url key gets DATABASE_URL with the branch DB swapped in, query kept;
# pgit itself connects to template1, else dropping the DB the url points at would fail
printf 'template: app\nenv:\n  file: .env.local\n  keys: [{url: POSTGRES_URL}]\n' > pgit.yaml
DATABASE_URL="$URL/app?sslmode=disable" "$B" create urlmode
eq "$(cat .env.local)" "POSTGRES_URL=$URL/db_branch_urlmode?sslmode=disable"
DATABASE_URL="$URL/db_branch_urlmode" "$B" drop urlmode -y
[[ ! -e .env.local ]] || fail "dropping the active DB should clear the url"

# connection.env is used over DATABASE_URL and must be set
printf 'connection:\n  env: PGIT_E2E_URL\n' >> pgit.yaml
must_fail "$B" list
PGIT_E2E_URL="$URL/app" DATABASE_URL=postgres://localhost:1 "$B" use
eq "$(cat .env.local)" "POSTGRES_URL=$URL/db_branch_feat_new"

echo "template: [" > pgit.yaml
git checkout -q main || fail "hook blocked checkout"

echo "e2e: all passed"
