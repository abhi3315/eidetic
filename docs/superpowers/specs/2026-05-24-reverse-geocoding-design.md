# Offline reverse geocoding

**Status:** Design draft 2026-05-24. Verification-gated. **All four V-claims PASS (re-verified 2026-05-24 through an end-to-end Rust harness against the live GeoNames mirror — see §Verification gate).** One docs-only correction: Himachal Pradesh's admin1 code is `IN.11`, not the earlier draft's `IN.08`.

> **Implementation note (2026-05-24, after the comprehensive-EXIF cleanup):** the project policy moved away from dedicated backfill subcommands. `Command::BackfillPlaces` was dropped before merge. `Geocoder::ensure_dataset` now runs from the `Import` command on first use; pre-existing rows without place columns get populated on reimport (`docker compose down -v` + reimport), the same pattern used after the `chore(cli): drop backfill-exif` change. The sections below describing `backfill-places` describe the original design, not what shipped.

**Working-state baseline:** `main` after the comprehensive-EXIF merge — 779 assets with EXIF, 553 of which carry `latitude` / `longitude`. No prerequisite PRs.

**Scope:**

1. **Local nearest-city lookup.** New `eidetic_core::geocoder` resolves `(lat, lon)` → `(country_code, country_name, admin1, place, distance_m)` using a bundled GeoNames `cities500` dataset. No third-party API call, ever. The user picked Eidetic explicitly because they don't want photo coordinates leaving their machine.
2. **Four new typed columns + one debug column on `assets`.** `country_code CHAR(2)`, `country_name TEXT`, `admin1 TEXT`, `place TEXT`, `place_distance_m REAL`. All nullable. Same backfill-then-import pattern as the comprehensive-EXIF PR.
3. **New `eidetic backfill-places` subcommand.** Iterates every row where `country_code IS NULL AND latitude IS NOT NULL`, runs the geocoder, UPDATEs. Same shape as `backfill-exif`.
4. **Import-path integration.** `import_file` populates the place columns inline when the local dataset is present. If the dataset is missing, import silently skips them — the backfill command will pick them up later.
5. **Detail-page surfacing.** New "Place" line above the existing GPS line on `/assets/:id`. Format: `Dalhousie, Himachal Pradesh, India (2.8 km)`. The distance is debug-friendly — flags cases where the matched city centroid is far from the photo.

**Out of scope** — explicit list in §Honest scope. Highlights: browse-by-place UI, map view, address-level resolution (street/house), reverse geocoding for non-GPS-tagged rows, anything that touches the network with the user's coordinates.

---

## Verification gate

Four load-bearing claims. V1 and V3 verified on 2026-05-24 against the user's real coordinates (12 distinct clusters in `assets`, all in Himachal Pradesh + one near Delhi) and against the latest cities500 download. V2 (bundling-strategy mechanics) and V4 (admin1 code → state-name resolution end-to-end through code) are mechanical re-confirmations the plan runs before any production code.

| # | Claim | If false | Verified 2026-05-24 |
|---|---|---|---|
| **V1** | GeoNames `cities500.txt` (~233K rows, 13 MB compressed) contains the small Indian towns in the user's library — Dalhousie (pop ~7600), Chamba (~21500), Dharamsala (~30700) — and resolves their coords to the right named place. | Switch to GeoNames `allCountries.zip` (~12M rows, 400 MB extracted; geocoder's lookup table will hate it). If that fails too, pivot to a different dataset (OpenStreetMap Nominatim DB extract, ~80 GB) — would force the bundling discussion into a different shape entirely and probably bump scope. | **PASS, re-verified 2026-05-24 through the V-gate harness against the live mirror.** Probed five user-coord clusters: (32.539,75.972) → Dalhousie, Himachal Pradesh, India (2794 m); (32.247,76.330) → Dharamsala, Himachal Pradesh, India (3136 m); (32.523,76.038) → Chamba, Himachal Pradesh, India (9038 m); (28.590,77.430) → Ghāziābād, Uttar Pradesh, India (8426 m); (30.087,78.267) → Birbhaddar, Uttarakhand, India (2267 m). Distances 2.3 km–9.0 km — reasonable. **Caveat retained:** the Rishikesh-area test resolves to "Birbhaddar" (a separate populated place 2.3 km from the test coord, vs Rishikesh 3.4 km away) — literal-nearest-neighbour, surfaced via `place_distance_m`. Population-aware coalescing is a documented follow-up (§Non-goals). |
| **V2** | A first-use download via `ureq` (pure-Rust HTTP + rustls) into `~/.cache/eidetic/geonames/` produces the same file the GeoNames mirror serves, can be detected as "already present" on subsequent runs, and the dataset is small enough (13 MB zip) to download in seconds over any reasonable connection. | Switch to committing `cities500.txt` directly to the repo (option (a) below). Adds ~25 MB to git, but no async/blocking dance, no network at first run, deterministic. | **PASS, re-verified 2026-05-24 through the V-gate harness.** `ureq::get(...).call()` + `zip::ZipArchive::by_name("cities500.txt")` + atomic `.partial` → rename downloaded the three files end-to-end in **8.5 s** on the user's connection. Sizes after extract: `cities500.txt` 40,326,055 bytes (from a 13,400,779-byte zip), `admin1CodesASCII.txt` 151,342 bytes, `countryInfo.txt` 31,678 bytes. Re-running `ensure_dataset` on a populated cache directory is a 3-`Path::exists`-check no-op (<0.1 ms). Mirror responded without auth or quota; the two small companion files served plain (not zipped). |
| **V3** | A naive O(N) haversine scan over the 233K-row cities500 dataset in release-mode Rust gives **< 10 ms per query**. Per-import overhead and 553-row backfill (~3 s) both stay invisible. | Build a sorted-by-latitude index with a 1° lat window — confirmed by the same probe to drop per-query to ~120 µs. Trivial fallback (~30 lines, no new deps). | **PASS, re-verified 2026-05-24 through the V-gate harness.** Same `Vec<City>` shape + linear haversine, run end-to-end including dataset load. Per-query (min of 5 iterations):<br><br>```<br>(32.539,75.972) Dalhousie  (2794m)  | scan=4.08ms<br>(32.247,76.330) Dharamsala (3136m) | scan=4.08ms<br>(32.523,76.038) Chamba    (9038m)  | scan=4.09ms<br>(28.590,77.430) Ghāziābād (8426m)  | scan=4.25ms<br>(30.087,78.267) Birbhaddar (2267m) | scan=4.26ms<br>```<br><br>Numbers improved vs the original probe (5–7 ms → 4 ms) — same algorithm, different CPU schedule. Still comfortably under the 10 ms target. Sorted-by-lat-window optimization remains a one-commit follow-up if ever needed. |
| **V4** | `country_code` (`IN`) → `country_name` (`India`) and `(country_code, admin1_code)` → `admin1_name` (`Himachal Pradesh`) resolve end-to-end through the geocoder's loaded lookup tables, not just at the spec-author's terminal. | Hand-curate a small admin1 lookup table for the countries the user actually shoots in. Trivial fallback if the GeoNames file format ever surprises us. | **PASS, re-verified 2026-05-24 through the V-gate harness.** Geocoder loaded 233,253 cities, 3,861 admin1 entries, 252 countries from the live GeoNames mirror in 159 ms. Resolution through `Geocoder.admin1.get(&("IN", "11"))` and `Geocoder.country.get("IN")`: `IN.11 → "Himachal Pradesh"`, `IN → "India"`, `US.CA → "California"`, `US → "United States"`. **Spec correction:** Himachal Pradesh's admin1 code in the real GeoNames file is `11`, not `08`. The earlier draft's "IN.08" was a typo — Uttarakhand similarly is `IN.39` → confirmed. Production code reads `admin1_code` directly from the city row, so this is a docs-only correction (no behaviour change). The Dalhousie cities500 row carries `IN\t\t11`, which resolves correctly through the loaded admin1 map. |

Plan Task 1 is exactly these four checks. V1 and V3 already record their verification; V2 and V4 are quick re-confirmations. No production code touched before Task 1.

---

## Goals

- Every asset with non-null `latitude` / `longitude` ends up with a country, state/province, and nearest-place name, plus the distance from the photo to that place's centroid.
- The geocoding runs entirely offline. No HTTP request involving photo coordinates, ever — first-use dataset download is the only network call, and it carries no user data.
- Backfill is one command: `eidetic backfill-places`. New imports populate the place columns inline when the dataset is present.
- The detail page shows a "Place" line above the GPS line: `Dalhousie, Himachal Pradesh, India (2.8 km)`.
- One migration, additive only. Zero risk to existing reads.
- `cargo clippy --workspace --all-targets -- -D warnings` green on every commit.

## Non-goals

- **Browse-by-place UI.** `eidetic browse --country IN --admin1 'Himachal Pradesh'` is a follow-up PR. The data plumbing lands here; the query surface arrives separately.
- **Map view.** Mapping needs tiles and a JS library; not in scope. The detail page stays text-only for place display.
- **Street-level / address-level resolution.** Real reverse geocoding ("123 Main St, Apt 4B") needs OSM Nominatim or equivalent — orders of magnitude more data, plus a much fuzzier matching problem. Defer.
- **Auto-network on import without consent.** If `~/.cache/eidetic/geonames/cities500.txt` isn't present at import time, the place columns stay NULL. Backfill runs explicitly (interactive command), where downloading 13 MB on first run is fine.
- **Coalescing GeoNames suburbs into their parent city.** V1's Birbhaddar-vs-Rishikesh caveat could be smoothed by a "if the nearest match is a `PPLX` (section of populated place) within 5 km of a `PPL` (populated place) with much higher population, prefer the bigger one" rule. That's a real future improvement; for v1 we just take the literal nearest and let `place_distance_m` show the truth.
- **Refreshing the dataset.** GeoNames updates daily. The first-use download is one-shot; the user can `rm -rf ~/.cache/eidetic/geonames` to force a re-download. No auto-refresh, no version pin.
- **GPS-direction → "facing east"** human-readable rendering. `gps_direction` is stored from the comprehensive-EXIF PR but not surfaced; out of scope here.

---

## Architecture overview

```
   import_file(path)                            backfill-places command
        │                                                 │
        │  exif.latitude.is_some()                        │  fetch_pending_places()
        │  && exif.longitude.is_some()                    │   (SELECT id, latitude,
        │                                                 │    longitude FROM assets
        ▼                                                 │    WHERE country_code IS NULL
   eidetic_core::geocoder::Geocoder::lookup(lat, lon)     │      AND latitude IS NOT NULL)
                       │                                  │
                       │                                  ▼
                       │                       for (id, lat, lon) in pending {
                       │                           Geocoder::lookup(lat, lon)
                       │                           repo.update_place_columns(id, place)
                       │                       }
                       ▼
              Option<Place> {
                  country_code: "IN",
                  country_name: "India",
                  admin1:       "Himachal Pradesh",
                  place:        "Dalhousie",
                  distance_m:   2794.0,
              }
```

Single seam, two callers (import + backfill). `Geocoder` is constructed once per process — it owns the loaded city array (~16 MB heap) and the country/admin1 maps. Construction is the slow part (load + parse ~233K rows, ~80 ms). Per-call `lookup` is the fast part (linear scan, 5–7 ms).

### `Geocoder` public API

```rust
// crates/eidetic-core/src/geocoder.rs

pub struct Geocoder {
    cities: Vec<City>,                      // ~233K rows, sorted by latitude
    admin1: HashMap<(String, String), String>, // (cc, code) → name
    country: HashMap<String, String>,       // cc → name
}

pub struct Place {
    pub country_code: String,
    pub country_name: String,
    pub admin1: String,
    pub place: String,
    pub distance_m: f32,
}

impl Geocoder {
    /// Construct a Geocoder by reading three GeoNames files from `data_dir`:
    /// `cities500.txt`, `admin1CodesASCII.txt`, `countryInfo.txt`. Use
    /// `Geocoder::default_data_dir()` for `~/.cache/eidetic/geonames/`.
    /// Returns `Err(GeocoderError::DatasetMissing)` if any file is absent —
    /// callers handle this by either downloading the dataset (CLI does this
    /// interactively in `backfill-places`) or skipping geocoding (the
    /// import path does this silently).
    pub fn open(data_dir: &Path) -> Result<Self, GeocoderError>;

    /// Download cities500.zip + admin1CodesASCII.txt + countryInfo.txt to
    /// `data_dir` if not already present. Synchronous; intended for the
    /// CLI to call before constructing the Geocoder. Prints progress to
    /// stderr.
    pub fn ensure_dataset(data_dir: &Path) -> Result<(), GeocoderError>;

    /// Return the default data dir: `~/.cache/eidetic/geonames/`.
    pub fn default_data_dir() -> PathBuf;

    /// Find the closest city by great-circle distance. Returns `None` only
    /// if `cities` is empty (which would mean an empty dataset file —
    /// loader rejects that as a parse error before this point).
    pub fn lookup(&self, lat: f64, lon: f64) -> Option<Place>;
}
```

Constructor returns `Result` because file IO can fail; `lookup` returns `Option` for forward-compat (a future empty-dataset case). Both are blocking; the import path wraps construction in `tokio::task::spawn_blocking`, the backfill CLI command builds the geocoder synchronously before entering the tokio loop.

### Why not a registry of geocoders / a trait

There's exactly one geocoder design in scope. Adding `trait Geocoder { fn lookup(...) }` plus a concrete `OfflineGeocoder` would set up indirection for zero callers. AGENTS.md: "Don't add a new abstraction (trait, generic, builder) for a single caller. Wait for two." If we later add (say) a Photon-via-private-LAN geocoder, that's when a trait is warranted. Today it's `pub struct Geocoder`, concrete.

---

## Dataset choice

GeoNames publishes graduated dumps. The relevant cuts:

| Dump | Rows | Compressed | Threshold | Verdict |
|---|---|---|---|---|
| `cities15000.txt` | ~26K | 1.6 MB | pop ≥ 15K | Misses Dalhousie (7K), Chamba (21K boundary), most of the user's photos. Too coarse. |
| `cities5000.txt` | ~52K | 2.7 MB | pop ≥ 5K | Catches Dalhousie. Misses Bakloh (1.8K) and the dense small-town Indian hill-station layer. |
| `cities1000.txt` | ~144K | 7.6 MB | pop ≥ 1K | Catches Bakloh. Misses Birbhaddar-style village suburbs. |
| **`cities500.txt`** | **~233K** | **13 MB** | **pop ≥ 500** | **Catches Birbhaddar (13K, but classed as a village). All five probed user-coords resolve correctly. Chosen.** |
| `allCountries.zip` | ~12M | 400 MB | none | Overkill — most rows are coordinates of farms, peaks, post offices. Lookup table size unmanageable. |

Going with cities500. The marginal cost over cities1000 is ~6 MB on disk and ~3 ms additional linear scan per query — both small, and we already need the small-town coverage for Indian hill stations.

If a future user has a library of remote-rural shots where even cities500 isn't fine-grained enough, switching to `allCountries` is a parameter change in `Geocoder::open` — same file format, same parser. Not anticipated for the personal-use library.

---

## Bundling strategy

Three options. Decision: **(b) first-use download.**

### (a) Commit `cities500.txt` (or `.zip`) to the repo

- **Pro:** zero-setup, deterministic CI, hermetic builds, works offline-by-default-for-real.
- **Con:** ~13 MB in git history forever (more if we ever bump GeoNames versions and the diff is large). Repo is currently ~30 MB checked-out + ~5 MB of git history; this would multiply both. The HEIC spec explicitly rejected committed fixtures for similar reasons; the DNG spec accepted a tiny (~5 KB) committed DNG. 13 MB sits in a different class.

### (b) First-use download via `ureq` into `~/.cache/eidetic/geonames/` — chosen

- **Pro:** repo stays small. Network call carries zero user data (just `GET cities500.zip`). Modelled on `eidetic-ml`'s `hf-hub`-based SigLIP weight download — same "lazy, cached, no auto-refresh" pattern, same cache root convention (`~/.cache/eidetic/<thing>/`).
- **Con:** Needs the network on first run. Adds `ureq` + `zip` deps to `eidetic-core` (the only crate that doesn't currently link tokio/sqlx/ort/reqwest). Both are pure-Rust, MIT-licensed, light (`ureq` ~50 KB compiled, ~10 transitive deps; `zip` similar). `ureq` blocking from inside tokio is handled by callers wrapping `Geocoder::ensure_dataset` in `spawn_blocking` (CLI doesn't need this; only the import path would, and we skip the auto-download from import anyway).
- **Trigger:** the `backfill-places` CLI command calls `ensure_dataset` before constructing the geocoder. If the dataset already exists, that's a fast existence check (3 stat() calls) and skip. If not, it downloads, extracts the zip, and verifies the three files are present. Print progress to stderr.

### (c) `build.rs` fetch — rejected

- Every clean `cargo build` re-downloads. CI cache helps but the trade-off is "no first-run download" vs "every fresh-clone download." Worse than (b).

### Eidetic-core dep budget

The user's CLAUDE.md design-approach line: "When tempted to add a new method or property, first ask 'can I remove something instead?'" Applied to deps: eidetic-core gains two new crates (ureq, zip). Both pure-Rust, both already permitted by `deny.toml`'s license allowlist (MIT/Apache-2.0). AGENTS.md restricts core from `tokio`/`sqlx`/`ort` specifically — `ureq` is sync HTTP, doesn't trip that rule. The cost is real but bounded; the alternative (committing 13 MB) is worse.

### Attribution

GeoNames is **CC-BY 4.0**. The README gets a line in the credits / acknowledgments section: `Geographic data © GeoNames, licensed under CC BY 4.0.` Per the licence, that's enough — no attribution needs to appear on every page that surfaces a place name. (We could also drop a tiny "GeoNames" line in the detail-page footer; deferred. The README acknowledgment is the licence-required spot.)

`deny.toml` doesn't list CC-BY-4.0 in its `[licenses].allow` list, but that's irrelevant — `cargo-deny` audits Rust crates, not data files. No deny.toml change required.

---

## Crate placement

The geocoder lives in **`eidetic-core::geocoder`**.

Reasoning (same chain as the DNG decision in `2026-05-23-dng-preview-design.md`):

- Two callers: `eidetic-ingest` (import path) and `eidetic-cli` (backfill subcommand). `eidetic-ingest` does NOT depend on `eidetic-cli`, and we don't want `eidetic-cli` to depend on `eidetic-ingest` for this (the backfill CLI just needs the geocoder, not the import pipeline). So the geocoder cannot live in either of those crates.
- `eidetic-core` already hosts pure-Rust foundation logic that multiple crates use (`Sha256`, `AssetId`, `dng::extract_largest_jpeg_preview`). Adding `geocoder` follows the same pattern. AGENTS.md's "lightweight foundation" rule restricts heavy deps; `ureq` + `zip` are arguably the heaviest new transitives core has gained (after kamadak-exif), but they're pure-Rust and well-bounded.
- A new `eidetic-geo` crate would have one type, one public function, and ~250 lines of code. Doesn't justify a workspace member per AGENTS.md's "must be at least one concrete caller in another crate before a new crate exists" rule (technically two callers, but both are well-served by adding to core; the mass isn't enough to fork).

`AGENTS.md` workspace-map row for `eidetic-core` gets updated: "...Also hosts `dng::extract_largest_jpeg_preview` ... and `geocoder::Geocoder` (offline nearest-city lookup from a GeoNames cities500 dataset cached under `~/.cache/eidetic/geonames/`)."

---

## Lookup data structure

Naive O(N) haversine scan over `Vec<City>`. Confirmed by V3 microbench: **5–7 ms per query** on the 233K-row dataset, which is well under the 10 ms target. For the 553-row backfill that's ~3 seconds total scan time. For per-import overhead it's well below the I/O cost of hashing and thumbnailing the same file.

`City` holds only the columns we read at query time: `name`, `lat`, `lon`, `country_code` (e.g., `"IN"`), `admin1_code` (e.g., `"11"` for Himachal Pradesh). Population, feature class, and alt names are parsed at load time only — used to verify cities500 contents, then dropped.

`lookup(lat, lon)` walks `self.cities`, tracks the running best by haversine distance, then resolves the winner's `country_code` against `self.country` and `(country_code, admin1_code)` against `self.admin1`. Missing names fall back to the raw codes. Returns `Place { country_code, country_name, admin1, place, distance_m: best_d as f32 }`.

A sorted-by-lat + 1°-window optimization drops query time from ~5 ms to ~120 µs (microbench: ~5K candidates per window vs ~233K without). It's ~30 lines on top of the linear scan. **Not in v1.** Linear scan is fast enough; the windowed version is a follow-up commit when the library crosses ~100K assets and the backfill starts being noticeable. Flagged in §Honest scope.

`haversine_m` is a 12-line standalone function (sin/cos/sqrt — no external dep needed; the `geo` crate is overkill for one haversine). Type returned in metres (`f64` internally for accuracy; cast to `f32` for the DB column since sub-metre precision is meaningless on photo GPS).

---

## Schema

One migration: `migrations/006_places.sql`.

```sql
-- Offline reverse-geocoded place columns. Populated by `eidetic
-- backfill-places` (one-time) and inline in the import path going forward.
-- All nullable; the import path skips geocoding silently if the
-- GeoNames dataset isn't cached locally.
--
-- country_code is ISO 3166-1 alpha-2 (e.g., "IN", "US"). CHAR(2) is the
-- right shape — the value is always exactly two ASCII letters or NULL.

ALTER TABLE assets
    ADD COLUMN country_code     CHAR(2),
    ADD COLUMN country_name     TEXT,
    ADD COLUMN admin1           TEXT,
    ADD COLUMN place            TEXT,
    ADD COLUMN place_distance_m REAL;
```

No indexes. No constraints. The 779 existing rows get NULL in every new column; backfill populates them. Index choices for the future browse-by-place query (`WHERE country_code = 'IN' AND admin1 = 'Himachal Pradesh'`) are deferred to that PR — premature to add now.

`place_distance_m` is included because it's load-bearing for debugging (without it, a "wrong-looking" place attribution is hard to diagnose) and surfaced inline on the detail page. `REAL` (= `f32`) is sufficient — sub-metre precision on a centroid distance from a photo's GPS is meaningless.

Per V4 in the comprehensive-EXIF spec, `ADD COLUMN ... NULL` is catalog-only on Postgres 11+. 779 rows is small enough that even a rewrite would finish before you noticed; plan Task 1 confirms with `\timing` on a seeded local DB.

### How the place columns get written

Two paths, mirroring how `latitude` / `longitude` / `camera_make` / etc. flow today:

1. **Import path:** `NewAsset` (in `crates/eidetic-db/src/assets.rs`) gains the five fields (`country_code`, `country_name`, `admin1`, `place`, `place_distance_m`, all `Option<...>`). `import_file` populates them from `Geocoder::lookup` before constructing `NewAsset`; `insert_asset` writes them in the single INSERT. No follow-up UPDATE, no race window where a row exists with NULL place data.
2. **Backfill path:** `update_place_columns(id, &Place)` UPDATEs the five columns for an existing row. Takes `&Place` directly — there is no separate "update payload" type; `Place` is both the geocoder's return value and the persistence shape, since their fields are identical.

`ExifUpdate` does **not** grow. The place columns are written through `update_place_columns` (separate from `update_exif_columns`) because (a) geocoding is independent of EXIF parsing — re-parsing EXIF doesn't change the place result, and re-geocoding doesn't change EXIF, and (b) the lifecycle is different: EXIF backfill runs once after files are imported, geocoding runs once after the dataset is downloaded. Coupling them in one update method would force re-running both when only one changes.

---

## Backfill subcommand

New `eidetic backfill-places [--dry-run]` subcommand. Same wiring pattern as `backfill-exif`.

### CLI shape

```
$ eidetic backfill-places
GeoNames dataset not found at ~/.cache/eidetic/geonames; downloading…
  cities500.zip (13 MB)... done
  admin1CodesASCII.txt (94 KB)... done
  countryInfo.txt (32 KB)... done
Geocoding 553 image assets with GPS data (226 rows skipped — no GPS)…
[ 50/553] IMG_3493.HEIC — Dalhousie, Himachal Pradesh, India (2.8 km)
[100/553] IMG_2238.JPG — Dharamsala, Himachal Pradesh, India (3.1 km)
…
[553/553] done. 553 rows updated.
```

A second invocation prints "0 pending; nothing to do." — `WHERE country_code IS NULL` matches zero rows.

### Why not auto-download from the import path

If you imported 10K photos and the dataset wasn't yet cached, the first import would block on a 13 MB download. That's surprising. The import path strictly does not initiate the download; it just calls `Geocoder::open` and if that returns `DatasetMissing`, it skips geocoding silently (logs at debug, leaves place columns NULL). The user then runs `eidetic backfill-places` explicitly, which downloads the dataset interactively (with progress to stderr) and populates every row that has GPS but no country_code.

### Repo methods (mirror `fetch_pending_exif_backfill` / `update_exif_columns`)

```rust
// crates/eidetic-db/src/assets.rs

/// Image rows with GPS data but no geocoded place. NULL marker is
/// "not yet processed"; backfill writes country_code for every row it
/// enumerates, so a subsequent run finds zero pending. Restricted to
/// `mime_type LIKE 'image/%'` — same convention as
/// `fetch_pending_exif_backfill` / `fetch_unembedded` / `fetch_unthumbnailed`.
/// GPS only lands on image rows today, but the filter keeps the query
/// honest if a future video format ever carries embedded coordinates.
pub async fn fetch_pending_places(&self) -> Result<Vec<(AssetId, f64, f64)>> {
    let rows: Vec<(uuid::Uuid, f64, f64)> = sqlx::query_as(
        "SELECT id, latitude, longitude FROM assets \
         WHERE country_code IS NULL \
           AND latitude IS NOT NULL AND longitude IS NOT NULL \
           AND mime_type LIKE 'image/%' \
         ORDER BY imported_at",
    ).fetch_all(&self.pool).await.map_err(Error::Query)?;
    Ok(rows.into_iter().map(|(u, lat, lon)| (AssetId::from(u), lat, lon)).collect())
}

/// UPDATEs the five place columns for an existing row. Takes
/// `&eidetic_core::geocoder::Place` directly — the geocoder's return
/// shape and the DB row's place columns are identical, so there is no
/// intermediate "update payload" type.
pub async fn update_place_columns(
    &self,
    id: AssetId,
    place: &eidetic_core::geocoder::Place,
) -> Result<()> {
    sqlx::query(
        "UPDATE assets SET \
           country_code = $2, country_name = $3, admin1 = $4, \
           place = $5, place_distance_m = $6 \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(&place.country_code)
    .bind(&place.country_name)
    .bind(&place.admin1)
    .bind(&place.place)
    .bind(place.distance_m)
    .execute(&self.pool).await.map_err(Error::Query)?;
    Ok(())
}
```

There is **no** separate `PlaceUpdate` struct. An earlier draft split `Place` (geocoder return) from `PlaceUpdate` (DB write payload) on principle, but their fields and types are identical, and the lifecycle distinction the comprehensive-EXIF spec uses for `ExifUpdate` (a separate type that can fold optional re-reads) doesn't apply here — geocoding produces all five fields or none. One type, owned by `eidetic-core::geocoder`, used by both call sites. `eidetic-db` takes `&eidetic_core::geocoder::Place` directly (the dep already exists in the workspace).

The CLI handler in `eidetic-cli/src/main.rs::Command::BackfillPlaces` mirrors the `BackfillExif` handler exactly: fetch pending, loop with progress, dry-run flag, error counts.

### `--dry-run`

Same shape as `backfill-exif`: runs `lookup` per row, prints "would set: place=Dalhousie, distance=2.8 km", does not UPDATE. Useful for confirming the geocoder behaves sensibly before scribbling over 553 rows.

### Rows without GPS

`fetch_pending_places` filters `AND latitude IS NOT NULL`. The 226 GPS-less rows are never enumerated; their `country_code` stays NULL permanently, by design. (When the future browse-by-place UI lands, those rows are filed under "Unknown location" or hidden depending on the query.)

---

## Going-forward import path

`crates/eidetic-ingest/src/import.rs::import_file` gains one block between the existing EXIF extraction (line 72, `let exif = if mime_type.starts_with("image/")`) and the `NewAsset` literal that feeds `repo.insert_asset` (literal begins line 107, call at line 132). The block produces `let place: Option<Place>` by matching on `(exif.latitude, exif.longitude)` — if both are `Some`, call `geocoder.lookup(lat, lon)`; otherwise `None`. The `match` is the only nontrivial bit:

```rust
let place = match (exif.latitude, exif.longitude) {
    (Some(lat), Some(lon)) => geocoder.and_then(|g| g.lookup(lat, lon)),
    _ => None,
};
```

`NewAsset` (in `crates/eidetic-db/src/assets.rs`) gains five new `Option<...>` fields — `country_code`, `country_name`, `admin1`, `place`, `place_distance_m` — mirroring how `latitude` / `longitude` / `camera_make` / etc. already flow. The construction site at line 107 sets them from `place.as_ref().map(|p| p.country_code.clone())` and so on, and `repo.insert_asset` writes them in the existing single INSERT. **No follow-up UPDATE; no race window** where the row briefly exists with NULL place data.

`insert_asset`'s INSERT statement (in `eidetic-db`) gains five columns and five bind parameters. Mechanical, same shape as how `gps_direction` was added in the comprehensive-EXIF PR.

### Geocoder lifecycle in the import path

`Geocoder::open` is ~80 ms of file IO + parse — fine for occasional one-off imports but pathological if called per file in a 10K-photo directory walk. Two options considered:

1. **Cache the `Geocoder` at the `import_dir` level.** `import_dir` opens it once at the top, threads `Option<&Geocoder>` into `import_file`. `import_file` becomes geocoder-aware.
2. **Lazy-init via `OnceLock`.** A `static GEOCODER: OnceLock<Option<Geocoder>>` initialised on first use. Simpler API but global state in a library crate — annoying for tests.

Choosing option 1. `import_file` gains a `geocoder: Option<&Geocoder>` parameter; callers that don't care pass `None`. Existing tests pass `None`; geocoder behaviour is unit-tested in `eidetic-core::geocoder::tests`, not in `import_file`.

The CLI's `Command::Import` handler constructs the geocoder once before walking the import path:

```rust
let geocoder = Geocoder::open(&Geocoder::default_data_dir()).ok();
// dataset not installed → None → skip geocoding silently
import_dir(&path, &repo, &config.paths, geocoder.as_ref()).await?;
```

---

## Detail page surfacing

`crates/eidetic-server/src/views.rs::detail_page` gains a "Place" block immediately before the existing GPS block (around line 218 today).

`DetailView` gains four fields:

```rust
pub(crate) country_name: Option<String>,
pub(crate) admin1: Option<String>,
pub(crate) place: Option<String>,
pub(crate) place_distance_m: Option<f32>,
```

(`country_code` is loaded into `AssetDetail` for the future browse-by-place URL pattern but NOT into `DetailView` — the rendered page doesn't need the ISO code.)

The new render block:

```rust
// Inside the dl, before the existing @if let (Some(lat), Some(lon)) = ... block:
@if let Some(p) = &view.place {
    dt { "Place" }
    dd {
        (p)
        @if let Some(a) = &view.admin1 { ", " (a) }
        @if let Some(c) = &view.country_name { ", " (c) }
        @if let Some(d) = view.place_distance_m {
            " (" (format_distance(d)) ")"
        }
    }
}
```

`format_distance` is a 4-line helper in `views.rs`: ≥ 1000 m renders as `X.X km` (1 decimal); below that, `N m` (no decimals).

The fields appear conditionally — a photo without `place` doesn't render the row at all. No empty "Place: —" line.

`fetch_by_id` in `eidetic-db/src/assets.rs` adds the five new columns to its SELECT, the inner `Row` struct, and the `AssetDetail` construction. `AssetDetail` gains `country_code`, `country_name`, `admin1`, `place`, `place_distance_m` to mirror. Server-side `handlers.rs::detail` (currently building `DetailView` at line 148) maps four of the five fields through to `DetailView` — `country_code` is loaded into `AssetDetail` for the future browse-by-place URL pattern but doesn't go into `DetailView`. This mapping is part of the same detail-page commit as the view-rendering changes; it does not land in an earlier commit. Same mechanical pattern as the comprehensive-EXIF PR's detail-page expansion.

The grid view (`fetch_recent` → `GridTile`) does **not** gain place columns in v1. Place is a per-tile filter target, not per-tile rendered content. Browse-by-place UI is the follow-up that touches the grid.

---

## Dependencies

**Workspace-level additions:**

```toml
# Pure-Rust HTTP client with rustls — used by eidetic-core::geocoder to
# download the GeoNames cities500 dataset on first run. Sync API; called
# from spawn_blocking on the import path and directly from the CLI's
# backfill command. Pinned to 2.x explicitly: ureq 3.x changed the
# request-builder / response-body API, and the dataset-download code
# (ureq::get(url).call()?.into_reader() into a tempfile) targets 2.x
# shapes. If we ever migrate to 3.x, that is a separate PR with its own
# verification, not a silent floor bump.
ureq = { version = "2", features = ["tls"], default-features = false }

# Zip decompression for the downloaded cities500.zip blob. Pure Rust.
zip = { version = "2", default-features = false, features = ["deflate"] }
```

Both are MIT-licensed (already on the allowlist; no `deny.toml` change). Pulled into `eidetic-core/Cargo.toml`:

```toml
ureq = { workspace = true }
zip = { workspace = true }
```

**Crate-level additions:**

- `crates/eidetic-core/Cargo.toml` — `ureq` + `zip` (above).
- `crates/eidetic-db/Cargo.toml` — no change. The DB crate doesn't reference Geocoder/Place; the new repo methods bind primitives.
- `crates/eidetic-ingest/Cargo.toml` — no change. Already depends on `eidetic-core`.
- `crates/eidetic-server/Cargo.toml` — no change.
- `crates/eidetic-cli/Cargo.toml` — no change. Already depends on `eidetic-core` and `eidetic-db`.

**Unchanged:**

- `kamadak-exif`, `serde_json`, `sqlx`, etc.
- System deps: zero new. No libheif analogue here; everything's pure-Rust.

---

## CI changes

None. The geocoder dataset isn't bundled in the repo, but tests can either (a) commit a tiny stub `cities500.txt` to `crates/eidetic-core/tests/fixtures/geonames/` (~3 cities total, hand-written; ~200 bytes), or (b) point unit tests at a one-off test fixture generated in `tempdir()`. Choosing (a) — a tiny committed fixture keeps tests hermetic and fast. License-clean by Abhishek's authorship of the 3-line file. CI doesn't need network access.

`cargo deny check` runs the same as today. New transitives (`ureq` → `rustls` + a handful, `zip` → `flate2` etc.) are all MIT/Apache-2.0/ISC; plan Task 1 confirms.

---

## README updates

Three additions:

1. **Subcommand list:** add `backfill-places` line.
2. **Attribution:** new "Acknowledgments" section (or extension of existing) with: `Geographic data © GeoNames, licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/).`
3. **Privacy note:** one paragraph explaining that reverse geocoding runs entirely on the local machine; the first-time download fetches the GeoNames dataset but no photo coordinates are ever sent over the network. Reinforces the "personal-use, no SaaS" stance from `goals.md`.

`AGENTS.md` workspace map: update `eidetic-core` row to mention `geocoder::Geocoder` (per §"Crate placement" above).

---

## Testing approach

### Unit tests in `eidetic-core::geocoder`

Lives in `crates/eidetic-core/src/geocoder.rs` under `#[cfg(test)] mod tests`. Fixtures at `crates/eidetic-core/tests/fixtures/geonames/`:

- `cities500.txt` — 3 hand-written rows in GeoNames tab-separated format: one row for Dalhousie (IN.11, 32.5394, 75.9719), one for Dharamsala (IN.11, 32.215, 76.319), one for San Francisco (US.CA, 37.7749, -122.4194). ~150 bytes.
- `admin1CodesASCII.txt` — 2 rows: `IN.11\tHimachal Pradesh\tHimachal Pradesh\t1`, `US.CA\tCalifornia\tCalifornia\t1`. ~80 bytes.
- `countryInfo.txt` — 2 non-comment rows: `IN\t...\tIndia\t...`, `US\t...\tUnited States\t...`. Format mirrors the real file (~20 tab-separated cols; only col 0 = code and col 4 = name are read).

Tests:

```rust
#[test]
fn open_and_lookup_resolves_dalhousie() {
    let g = Geocoder::open(Path::new("tests/fixtures/geonames")).unwrap();
    let p = g.lookup(32.539, 75.972).unwrap();
    assert_eq!(p.country_code, "IN");
    assert_eq!(p.country_name, "India");
    assert_eq!(p.admin1, "Himachal Pradesh");
    assert_eq!(p.place, "Dalhousie");
    assert!(p.distance_m < 5_000.0);
}

#[test]
fn lookup_returns_admin1_code_when_admin1_name_missing() {
    // Tests the fallback when admin1CodesASCII doesn't have a matching row.
    // Hand-crafted geocoder with a city pointing at an unknown admin1.
}

#[test]
fn open_returns_dataset_missing_when_files_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        Geocoder::open(tmp.path()),
        Err(GeocoderError::DatasetMissing)
    ));
}

#[test]
fn haversine_known_distances() {
    // SF to NYC: ~4135 km. SF to LA: ~559 km.
    let m = haversine_m(37.7749, -122.4194, 40.7128, -74.0060);
    assert!((m - 4_135_000.0).abs() < 10_000.0);
}
```

### Integration test for `ensure_dataset`

Skipped from CI (would need network). Manual: run `eidetic backfill-places` on a fresh `~/.cache/eidetic/geonames/` and confirm the three files land. Documented in the smoke test below.

### `update_place_columns` repo test

In `crates/eidetic-db/tests/places.rs` (new file) — testcontainers Postgres, insert an asset row, call `update_place_columns` with an `eidetic_core::geocoder::Place`, SELECT back, assert each field. Same shape as the existing EXIF repo test pattern.

### Server-side detail-page test

In `crates/eidetic-server/src/views.rs::tests`:

- `detail_page_renders_place_block` — `DetailView { place: Some("Dalhousie"), admin1: Some("Himachal Pradesh"), country_name: Some("India"), place_distance_m: Some(2794.0), .. }` produces HTML containing `"Dalhousie"`, `"Himachal Pradesh"`, `"India"`, `"2.8 km"`.
- `detail_page_omits_place_when_none` — `DetailView { place: None, .. }` produces HTML without the literal `"Place"` `dt` label.

### What we don't unit-test

- The 233K-row real dataset. Manual smoke test below.
- The network download path (`ensure_dataset`). Manual.
- Microbenchmark of lookup speed on the real dataset. Plan Task 1 re-runs the standalone Rust microbench used in V3.

### Manual smoke test (post-merge)

1. `rm -rf ~/.cache/eidetic/geonames` (force a fresh download).
2. `cargo run -p eidetic-cli -- backfill-places --dry-run` → first prints the download progress, then "Would update 553 assets" with no DB writes.
3. `cargo run -p eidetic-cli -- backfill-places` → per-row progress with realistic place names. `553/553 done. 553 rows updated.`
4. `docker exec eidetic-db-1 psql -U eidetic -d eidetic -c "SELECT place, admin1, country_name, COUNT(*) FROM assets WHERE place IS NOT NULL GROUP BY 1,2,3 ORDER BY 4 DESC LIMIT 10"` → sane Indian hill-station results.
5. `cargo run -p eidetic-cli -- serve` → open `/assets/<some_id>` → "Place: Dalhousie, Himachal Pradesh, India (2.8 km)" appears above the GPS line.
6. `cargo run -p eidetic-cli -- backfill-places` again → "0 pending; nothing to do." (Idempotency.)
7. `cargo run -p eidetic-cli -- import /Volumes/Data/Dalhousie\ /more-photos/` → newly-imported rows get place columns populated inline (because the dataset is now cached).

---

## Failure-mode semantics

Five buckets:

1. **Dataset missing on disk** (`~/.cache/eidetic/geonames/` doesn't have all three files). Import path: log at debug, skip geocoding, leave place columns NULL. CLI backfill: trigger `ensure_dataset` to download.
2. **Dataset download fails** (offline, GeoNames mirror down, DNS broken). `ensure_dataset` returns `GeocoderError::DownloadFailed`. CLI's `backfill-places` prints the error and exits non-zero. No partial state — the cache dir is either complete or absent. Implementation: write to `~/.cache/eidetic/geonames/cities500.txt.partial`, rename on success, delete on failure. (Standard atomic-file-write pattern.)
3. **Dataset present but parse-error** (corrupt file, GeoNames format changed unexpectedly). `Geocoder::open` returns `GeocoderError::Parse { path, line, source }`. Import path skips geocoding; backfill exits non-zero with the diagnostic. Manual recovery: `rm -rf ~/.cache/eidetic/geonames && eidetic backfill-places` to re-download.
4. **A row's `(lat, lon)` is bogus** (e.g., `(0.0, 0.0)` from a broken EXIF). The geocoder still returns *something* — the nearest city to (0, 0) is somewhere in the Gulf of Guinea. We write that result honestly. The `place_distance_m` column will be huge (~hundreds of km), which makes the row visible as suspicious. Filtering bogus coordinates is the EXIF extractor's job, not the geocoder's.
5. **Geocoder constructed but lookup somehow returns `None`** (the dataset loaded but was empty). `update_place_columns` simply isn't called; row stays NULL. Backfill increments a "no result" counter for the summary. This case is defensive — the loader rejects an empty file as a parse error before we get here.

No `unwrap`, no row loss, no UPDATE that writes a partial mess back to the DB. Same convention as the rest of the codebase (AGENTS.md "Errors").

---

## Risks

### Privacy regression risk

The whole point of this PR is that photo coordinates never leave the machine. One slip — accidentally calling `https://nominatim.openstreetmap.org/...` from a debug helper, or shipping a test that hits a network endpoint with a real lat/lon — undoes the design intent.

**Mitigation:** the only network call in the entire change is `GET https://download.geonames.org/export/dump/cities500.zip` (and the two small companion files). This URL appears in exactly one place — `eidetic_core::geocoder::ensure_dataset`. The URL is a constant string with no parameter interpolation; it cannot leak a user coordinate. Plan Task 1 grep-confirms zero other HTTP-call sites in the diff. The README's privacy paragraph documents this externally.

### GeoNames mirror availability

`download.geonames.org` has been stable for 15+ years (the project predates 2007), but it's a single point of failure for first-time setup. If the mirror goes down, the user can't run `backfill-places` until either (a) the mirror comes back, or (b) they manually populate `~/.cache/eidetic/geonames/`. Both are fine for personal use; flagged so a future contributor knows.

A future improvement: support pulling from a HuggingFace mirror (we already have `hf-hub` for the SigLIP weights). Out of scope here; documented in §Honest scope.

### Suburb-vs-city ambiguity (the Birbhaddar problem)

V1's PASS is honest: GeoNames thinks Birbhaddar is a separate populated place from Rishikesh. Our literal nearest-neighbour search picks the closer centroid. Sometimes that's a suburb, sometimes that's a smaller adjacent town. For a personal photo library, "the photo was taken in Birbhaddar (or Rishikesh, 1km north)" is the truth; both labels would be fine but only one fits in `place`.

If this turns out to be jarring in real use, the future fix is a population-aware coalescing rule (described in §Non-goals). For v1: take the literal nearest, surface `place_distance_m` so the user can spot oddities, write up the trade-off honestly in the spec self-review.

### `ureq` + `zip` toolchain mass in eidetic-core

The user's CLAUDE.md design rule: "before coding, trace the full execution lifecycle." Applied here: `eidetic-core` was kept light specifically because every other crate depends on it; pulling in `ureq` (with `rustls` underneath) and `zip` (with `flate2`) bumps that base. The cost on a fresh `cargo build`: ~5–10 seconds of additional compile time across rustls + miniz_oxide + their transitives. For an editor's incremental build that's invisible; for fresh CI runs it's borderline-noticeable. Acceptable trade-off against committing 13 MB to git, but the alternative (option (a)) stays on the table for future revisiting if the dep mass becomes a real friction point.

### Dataset staleness

The downloaded dataset is "as of the day you first ran `backfill-places`." If a town gets renamed in 2027, the cache still says the old name. For a personal photo library — where the bigger truth is "which Indian state am I in?" rather than "what's the latest name of this village?" — staleness is fine. The user can `rm -rf ~/.cache/eidetic/geonames` to refresh whenever they want. Auto-refresh deferred.

---

## Honest scope

### In v1 (this PR)

- One migration adding five nullable columns to `assets` (`country_code`, `country_name`, `admin1`, `place`, `place_distance_m`).
- `eidetic_core::geocoder` module: `Geocoder::open`, `Geocoder::ensure_dataset`, `Geocoder::default_data_dir`, `Geocoder::lookup`. Loader for the three GeoNames files. Linear-scan haversine lookup.
- Two workspace deps: `ureq`, `zip`. Both pure-Rust, MIT.
- `import_file` takes an optional `&Geocoder` and populates place columns inline when present.
- New `Command::BackfillPlaces { dry_run }` in `eidetic-cli`. Calls `ensure_dataset` to download on first run.
- Detail page renders "Place" line above the "GPS" line.
- Unit tests in `eidetic-core::geocoder` against a 3-row committed fixture.
- Server-side `detail_page` tests for the place block (rendered + omitted cases).
- DB integration test for `update_place_columns`.
- README acknowledgment + privacy note; AGENTS.md workspace-map update.

### Deferred (call out explicitly)

- **Browse-by-place UI.** `eidetic browse --country IN --admin1 'Himachal Pradesh'` + grid filters on the server. Separate PR; data plumbing here.
- **Map view in the web UI.** Needs tiles, JS, layout work.
- **Address-level resolution.** Different problem; OSM Nominatim or equivalent.
- **Admin2 (district) column.** Would need `admin2Codes.txt` from GeoNames. The user explicitly punted this in the conversation; not in v1.
- **Population-aware suburb-vs-city coalescing** (the Birbhaddar fix).
- **Dataset auto-refresh.** User-initiated via `rm -rf` for v1.
- **Sorted-by-lat-with-window index.** Pure performance optimization; not needed at 553 rows.
- **GPS-direction "facing east" rendering** for the existing `gps_direction` column.
- **Alternative dataset sources** (HuggingFace mirror, OSM extract).
- **Coordinate-bogus detection.** A `latitude IN (0, 0)` heuristic + an EXIF-extraction warning would catch many bad-GPS rows. Out of scope.

---

## Implementation order (hint for plan author)

1. **Verification gate** (Plan Task 1). Re-run V1 + V3 (already PASS — re-confirm the microbench compiles + matches recorded numbers), run V2 (download the dataset to a tempdir, confirm three files extract correctly, verify a re-run is a fast no-op), run V4 (parse the same files through `Geocoder::open` and `lookup`, confirm `IN.11 → Himachal Pradesh` resolves through Rust code, not just grep). Record findings as inline updates to the V-gate table in §"Verification gate". **(Completed 2026-05-24 — all four PASS. V4 surfaced one docs-only typo: spec said `IN.08` for Himachal Pradesh, real GeoNames uses `IN.11`; updated in place. No code impact.)**
2. **Migration + schema-only DB changes** (atomic commit). `migrations/006_places.sql`. Extend `AssetDetail` and the inner `Row` struct in `fetch_by_id` with the five new fields and SELECT list. Extend `NewAsset` with the five new `Option<...>` fields and bind them in `insert_asset`'s INSERT. Add `fetch_pending_places` (mime-filter included; doesn't reference `Place`). **Defer** `update_place_columns` to step 3 — it takes `&eidetic_core::geocoder::Place`, which doesn't exist yet. Clippy green; no behaviour change (every new `NewAsset` field is `None` at every call site; `fetch_pending_places` is unused for one commit — `#[allow(dead_code)]` is acceptable, or roll into step 3 if that feels cleaner).
3. **`eidetic-core::geocoder` module + `update_place_columns`** (atomic commit). Workspace deps (`ureq`, `zip`). Loader for the three GeoNames files. `Geocoder` struct + `Place` struct + `GeocoderError` enum. Linear-scan `lookup`. `default_data_dir`. Public API per §"Architecture overview". Add `update_place_columns(id, &Place)` to `eidetic-db` (its caller arrives in step 6; `#[allow(dead_code)]` for two commits is acceptable). Unit tests against the committed 3-row fixture. Clippy green.
4. **`Geocoder::ensure_dataset`** (atomic commit). Download via `ureq` to `.partial` files, extract via `zip`, atomic-rename. Progress messages to stderr. No new tests in this commit (network-dependent); manual smoke covers it. Clippy green.
5. **Import-path integration** (atomic commit). Thread `Option<&Geocoder>` through `import_file` and `import_dir`. CLI's `Command::Import` constructs the geocoder once at the top and passes it down. Existing tests of `import_file` pass `None`. Clippy green; existing tests still green.
6. **`Command::BackfillPlaces`** (atomic commit). New variant in `Command` enum. Handler in `main.rs` mirrors `Command::BackfillExif` — calls `ensure_dataset`, constructs `Geocoder`, calls `fetch_pending_places`, loops with progress, dry-run flag. Clippy green.
7. **Detail-page rendering** (atomic commit). Extend `DetailView` with the four new fields. Add the "Place" block to `detail_page` per §"Detail page surfacing". Wire fields through `handlers.rs`. Two new view tests. Clippy + tests green.
8. **README + AGENTS.md** (atomic commit). Subcommand list, acknowledgment, privacy paragraph. Workspace-map update for `eidetic-core`.
9. **Open PR on `feat/reverse-geocoding` worktree.** On Abhishek's Mac, run the smoke test from §Testing approach. Confirm the place columns are populated for the 553 GPS rows, and that the detail page renders "Dalhousie, Himachal Pradesh, India" above the GPS coords. Record findings in PR description.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green. Pre-commit hook enforces this. Commit messages follow Conventional Commits (`feat`, `chore`, `test`, `docs`); no `Co-Authored-By`.

---

## Decisions resolved (formerly open questions)

1. **Dataset choice.** `cities500.txt` (~233K rows, 13 MB zip). cities1000 misses Birbhaddar-style suburbs; cities5000 misses Indian hill-station towns like Bakloh.
2. **Bundling strategy.** First-use download via `ureq` into `~/.cache/eidetic/geonames/`. Matches the SigLIP weights pattern in `eidetic-ml`. Repo stays small.
3. **Crate placement.** `eidetic-core::geocoder`. Two callers (ingest + cli), neither can host the other. Adds `ureq` + `zip` to core — accepted as a real but bounded cost.
4. **Lookup data structure.** Linear-scan haversine over `Vec<City>`. V3 microbench confirms 5–7 ms per query. Sorted-by-lat-window optimization is a one-commit follow-up if ever needed.
5. **Column choice.** Five columns: `country_code` (ISO), `country_name`, `admin1` (state/province name), `place` (nearest city/town), `place_distance_m` (debug-friendly). No `admin2` in v1.
6. **Backfill vs re-import.** Backfill subcommand. Re-importing 6 GB of files to populate 5 small columns is wasteful and the geocoding lifecycle is independent of EXIF parsing.
7. **Persistence shape.** `eidetic_core::geocoder::Place` serves as both the geocoder's return value and the DB write payload — fields are identical, no separate `PlaceUpdate` struct. The import path writes the five place fields through `NewAsset` (one INSERT, no race window); the backfill path writes them through `update_place_columns(id, &Place)` (a separate UPDATE because backfill operates on existing rows). `ExifUpdate` does not grow — geocoding and EXIF parsing have independent lifecycles, so they get separate update methods.
8. **Detail-page placement.** "Place" block above the "GPS" block (the user's preferred position from the conversation).
9. **Auto-download on import.** No. The import path uses an already-cached dataset if present, skips silently otherwise. The CLI's `backfill-places` does the interactive download.
10. **CC-BY 4.0 attribution.** README "Acknowledgments" line. No per-page attribution needed under the license.

---

## Spec self-review

**Placeholder scan.** No TBDs. Every column has a source mapping, every code change has either the literal Rust or a clear file pointer (`views.rs::detail_page` near `dt { "GPS" }`). V2 and V4 are mechanical re-confirmations the plan handles before any code.

**Internal consistency.**
- "Five new columns" matches §Schema, §"Detail page surfacing", §"Backfill subcommand", §"Decisions resolved" #5.
- "Linear-scan O(N) lookup" matches §"Lookup data structure", §"Verification gate" V3, §"Decisions resolved" #4.
- "First-use download via ureq" matches §"Bundling strategy", §Dependencies, §"Architecture overview" (where `ensure_dataset` is named), §"Decisions resolved" #2.
- "No auto-download from import" matches §"Going-forward import path", §"Failure-mode semantics" #1, §"Risks" / privacy paragraph, §"Decisions resolved" #9.
- "Place block above GPS block" matches §"Detail page surfacing", §"Decisions resolved" #8.
- "Single `Place` type, no `PlaceUpdate`" matches §Schema (How the place columns get written), §"Backfill subcommand" (repo methods), §"Decisions resolved" #7. `NewAsset` gains five place fields, persisted through `insert_asset`; `update_place_columns(id, &Place)` is the backfill seam.
- "`mime_type LIKE 'image/%'` in pending queries" matches §"Backfill subcommand" / `fetch_pending_places`; same convention as `fetch_pending_exif_backfill` at `assets.rs:231`.

**Scope.** Single PR. One migration (5 SQL lines), one new module in `eidetic-core` (~250 lines including unit tests + a 3-row fixture file), one CLI handler (~80 lines, mirrors `BackfillExif`), one detail-page block (~12 lines), `NewAsset` gains five `Option<...>` fields + `insert_asset` INSERT extension + 2 new repo methods in `eidetic-db` (~50 lines), one workspace dep block. Nine commits per §"Implementation order". Similar size to the comprehensive-EXIF PR; smaller than HEIC.

**Ambiguity flagged.** V1's Birbhaddar caveat is the one nuance — recorded honestly in the V-gate annotation and §"Risks". `ureq` is pinned to `2.x` deliberately (see §Dependencies comment); plan Task 1 confirms the dataset-download code shape compiles against that line, no version-floor surprises.

**Known risk.** See §"Risks". Self-review intentionally does not restate.

**Spec coverage check.**

Mapping the user's original ask to spec sections:

- "Offline only, no third-party API calls, bundle a GeoNames-style local dataset" → §"Scope", §"Bundling strategy", §"Risks" / privacy paragraph ✓
- "Pure-Rust deps, MIT/Apache-2.0 only" → §Dependencies (ureq + zip, both MIT, pure-Rust) ✓
- "Privacy-clean dataset (GeoNames CC-BY 4.0)" → §"Bundling strategy" / attribution, §"README updates" ✓
- V1 probe against Indian coords → §"Verification gate" V1 ✓
- V2 dataset size + commit-vs-download argument → §"Bundling strategy" ✓
- V3 lookup speed claim → §"Verification gate" V3 ✓
- V4 admin1 resolution → §"Verification gate" V4 ✓
- New columns `country_code / country_name / admin1 / place / place_distance_m` → §Schema ✓
- `eidetic backfill-places [--dry-run]` → §"Backfill subcommand" ✓
- Import-path inline population → §"Going-forward import path" ✓
- Detail page "Place" line above GPS → §"Detail page surfacing" ✓
- Crate placement in `eidetic-core::geocoder` → §"Crate placement" ✓
- Public API minimal (`open`, `ensure_dataset`, `default_data_dir`, `lookup`) → §"Architecture overview" / "Geocoder public API" ✓
- Out of scope: browse-by-place, map view, address-level → §"Non-goals", §"Honest scope" ✓
- Quality bar: workspace clippy green, atomic commits, Conventional Commits, no Co-Authored-By → §"Implementation order" ✓
- Develop on `feat/reverse-geocoding` worktree, don't push to main → §"Implementation order" step 9 ✓
