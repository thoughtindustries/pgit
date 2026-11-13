# pgit

Per-branch Postgres databases. `pgit create` clones your base database for the current git branch and points your app at it, by writing `DB_NAME` (or whatever keys you choose) to `mise.local.toml`, `fnox.local.toml` or a `.env` file.

Single binary, no psql/libpq needed.

## Install

With Homebrew (builds from source):

```sh
brew tap thoughtindustries/pgit https://github.com/thoughtindustries/pgit.git
brew install pgit           # latest tag; --HEAD for main
brew upgrade pgit           # later, to update
```

Or with cargo: `cargo install --git https://github.com/thoughtindustries/pgit`

## Quick start

```sh
pgit init                 # writes pgit.yaml, edit `template`
git checkout -b feature/TI-123-audit
pgit create               # db_branch_feature_ti_123_audit, cloned from template
pgit list
pgit reset                # back to the template database
pgit drop                 # drop the current branch's database
```

## Commands

| Command | |
|---|---|
| `create [branch] [--from <branch>] [-f]` | Clone the template (or another branch's DB) and switch to it. `-f` terminates connections to the source. |
| `use [branch]` | Switch to an existing branch database. |
| `drop [branch] [-y]` | Drop a branch database (asks first). |
| `list` | Branch databases, sizes, `*` = active. |
| `reset` | Remove the override, back to the template. |
| `status` | Config, server, active DB, DB for the current branch. |
| `init` | Write a starter `pgit.yaml`. |
| `prune [-y] [--dry-run]` | Drop databases whose git branch no longer exists. |
| `hook install` | Install a `post-checkout` hook that follows branch switches. `pgit hook` prints the line for husky/lefthook. |

`branch` always defaults to the current git branch.

## pgit.yaml

pgit looks for `pgit.yaml`, `.pgit.yaml`, `.config/pgit.yaml` or `.config/.pgit.yaml` from the current directory up to the git root.

```yaml
template: postgres               # required: base database to clone
name: "db_branch_{branch}"       # {branch} = sanitized branch name (lowercase, [a-z0-9_])
extract: '([A-Z]+-\d+)'          # optional: use this capture instead of the whole branch
env:
  file: mise.local.toml          # [.]mise.local.toml = mise [env], [.]fnox.local.toml = fnox [secrets], else dotenv
  keys:                          # each key gets the database name or the connection url
    - name: DB_NAME
    # - url: DATABASE_URL        # the connection url with the database swapped in
connection:
  url: postgres://postgres:password@localhost:5432
  # fnox: DATABASE_URL           # or: read the url from this fnox secret (`fnox get`)
  # env: POSTGRES_URL            # or: read the url from this env var
hook: use                        # on checkout: use (switch if it exists, else reset) | create
```

Connection precedence: `connection.url`, `connection.fnox` or `connection.env` (only one) > `DATABASE_URL` > `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD` > `postgres@localhost:5432`. The database in the url is ignored; pgit connects to `template1`.

If your app reads one full url (e.g. `POSTGRES_URL=postgres://u:p@host/app`) instead of a database name, add `- url: POSTGRES_URL` to `env.keys`. pgit then writes the connection url with the branch database swapped in, credentials and query params kept. `name:` and `url:` keys can be mixed; every key is written on `create`/`use` and removed on `reset`, so your base value (e.g. in `.env`) applies. pgit reads the first key to tell which database is active.

## Safety

- pgit only lists, drops and prunes databases that match your `name` template **and** carry this repo's marker (`COMMENT ON DATABASE … 'pgit:<repo git dir>:<branch>'`), so repos sharing a server with the same template never see each other's databases. Databases created before pgit that match the template prefix show up as `(legacy)`: `drop` works on them, `prune` never touches them. So do databases whose recorded repo was moved or deleted (its `.git` is gone but the folder that held the repo still exists); paths that only exist elsewhere (another machine, a container) stay invisible. The `template` database is never listed or dropped.
- pgit owns its env keys in `env.file`: if one holds something that isn't a branch database name (say your own `DB_NAME`), pgit refuses to touch the file instead of overwriting or removing it.
- `prune` keeps a database while its branch exists or any local branch maps to it (with `extract`, several branches can share one).
- Per-database settings of the template (`search_path`, …) are copied to the clone, since `CREATE DATABASE … TEMPLATE` doesn't.
- The git hook never blocks a checkout; failures are printed as warnings.

## Limits

No TLS and no `.pgpass` support yet; pgit targets local development servers. Needs Postgres 13+ (`DROP DATABASE … WITH (FORCE)`).

## Development

Tasks run through [mise](https://mise.jdx.dev), which also installs the Rust toolchain:

```sh
mise run dev status     # run pgit from source with any args
mise run build          # release binary in target/release/pgit
mise run test           # unit tests
mise run lint           # fmt check + clippy
mise run e2e            # end-to-end against a throwaway Postgres container (needs docker + psql)
mise run check          # all of the above, like CI
```

## License

Copyright (C) 2026 Thought Industries

pgit is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. See [LICENSE](LICENSE).
