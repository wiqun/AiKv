# AiDb v0.6.3 Integration Notes (2026-01-09)

Summary
- Upgraded dependency: `aidb` -> `v0.6.3`.
- Root fix: MemTable tombstone visibility + `DB::get()` behavior (resolved an issue where tombstone in MemTable was indistinguishable from not-found and old SSTable values could be returned).
- Validation: Rebuilt cluster with `aikv-tool` and ran the TL.Redis test suite; all 63 tests passed locally.

Problem details
- Before v0.6.3: `MemTable::get()` returned `None` for tombstones; DB read path treated `None` as "not found" and continued to scan SSTables. If an SSTable contained an older value, it would be returned, causing `DEL` to appear successful (returned 1) while `EXISTS` still returned true.
- After v0.6.3: `DB::get()`/internal path has been fixed so a tombstone blocks older SSTable values being returned. This restores correct semantics for `DEL`/`EXISTS` and fixes intermittent wrong-type errors in tests that relied on immediate visibility of delete operations.

Repro & Validation Steps
1. Build and run fresh cluster:
   - `./aikv-toolchain/target/release/aikv-tool cluster stop -v`
   - `./aikv-toolchain/target/release/aikv-tool cluster setup`
2. Run tests:
   - `cd /path/to/TL.Redis`
   - `dotnet test`
3. Confirm: previously failing tests referencing keys like `test001:{0}` now pass and `DEL` + `EXISTS` behave correctly in manual `redis-cli` checks.

Notes for Developers
- We bumped `Cargo.toml` in the project to reference `aidb = { git = "https://github.com/wiqun/AiDb", tag = "v0.6.3" }`.
- No changes to AiKv command APIs were required besides the dependency update and the documentation entry.
