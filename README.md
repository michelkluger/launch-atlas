# Launch Atlas

A hike-and-fly planner for Switzerland: ranks paragliding launches by the ratio
of glide reach to hiking effort, with real swisstopo data for terrain, BAFU
restrictions, BAZL airspace and aviation obstacles, OSM hiking trails, and
cable-car access.

Built in Rust + a single static HTML/Leaflet frontend served by `axum`.

![Launch Atlas screenshot](docs/screenshot.png)

## Features

- **Real Swiss DEM** — SwissALTI3D 2 m tiles fetched per peak via the swisstopo
  STAC API, decimated to 25 m, stitched into one binary blob.
- **Terrain-following glide flood** — single-source Dijkstra per launch, max
  AGL altitude per cell, configurable glide ratio (4–12).
- **Wind-aware glide path** — direction-dependent ground glide ratio
  (`(airspeed + tailwind) / sink`); reachability footprints elongate
  downwind, headwind kills upwind progress.
- **Trail-aware hike cost** — OSM `highway=path` with `sac_scale` filter,
  Schweizer Wanderwege time formula (4 km/h horiz, 400 m/h ↑, 600 m/h ↓,
  `max+min/2`).
- **Cable cars / funiculars** — OSM `aerialway` + `railway=funicular|rack_railway`,
  clustered into lift lines, used as fast valley-to-peak access seeds.
- **No-fly filter** — drops launches inside Wildruhezonen (BAFU), federal
  hunting reserves, national parks, CTRs (BAZL).
- **Aviation obstacles** — BAZL `Luftfahrthindernis` cable spans + cranes
  rendered as map overlays.
- **Pareto-frontier scoring** — `glide_value / hike_cost^α × cos(Δ_wind)`,
  with α slider for "I'll happily hike further" tuning.

## Build

```bash
cargo build --release
```

Binaries land in `target/release/`:

| Binary | Purpose |
|---|---|
| `launchatlas` | Web server (port 3000) |
| `launchatlas-fetch-dem` | SwissALTI3D DEM fetch (per-peak bboxes) |
| `launchatlas-fetch-lifts` | OSM cable cars / funiculars |
| `launchatlas-fetch-trails` | OSM hiking trails (SAC ≥ T2 by default) |
| `launchatlas-fetch-warnings` | BAFU + BAZL polygons + obstacles |
| `launchatlas-demo` | Synthetic-DEM CLI smoke test |

## Setup data (one-time, ~5–10 minutes)

```bash
# 1. Fetch peaks list (130 OSM peaks 1700–2800 m, BE area, ≥2 km apart):
#    Run the helper or supply your own peaks-bern.json
#    See PEAKS.md for the exact Overpass query.

# 2. Fetch the DEM (130 peaks × ~6 tiles each ≈ 800 unique tiles, ~700 MB cache):
./target/release/launchatlas-fetch-dem --peaks peaks-bern.json --cell 25

# 3. Fetch OSM lifts (~260 entries):
./target/release/launchatlas-fetch-lifts

# 4. Fetch SAC ≥ T2 trails (~5000 segments, ~10 MB):
./target/release/launchatlas-fetch-trails

# 5. Fetch BAFU + BAZL polygons + obstacles:
./target/release/launchatlas-fetch-warnings

# 6. Run the server:
./target/release/launchatlas
# Open http://127.0.0.1:3000
```

## Architecture

Cargo workspace, ~5 k LOC:

```
crates/
├── core           types: Dem, CellIdx, LV95, Launch, Wind
├── data           swisstopo + OSM ingest (STAC, API3, Overpass)
├── terrain        Horn slope/aspect, planar-fit roughness
├── launch         auto-discover candidate launches by slope/aspect
├── glide          single-source Dijkstra-flood, wind-aware
├── hike           Tobler hike-time field with seeded initial costs
├── score          Pareto frontier, cos(Δ_wind) factor
├── api            axum server + static UI
├── cli            synthetic-DEM smoke demo
├── fetch          launchatlas-fetch-dem
├── fetch-trails   launchatlas-fetch-lifts
├── fetch-paths    launchatlas-fetch-trails
└── fetch-warnings launchatlas-fetch-warnings
```

## Data sources & licenses

| Layer | Source | License |
|---|---|---|
| DEM | swisstopo SwissALTI3D | OGD swisstopo |
| Wildruhezonen | BAFU | OGD swisstopo |
| Hunting reserves | BAFU `bundesinventare-jagdbanngebiete` | OGD swisstopo |
| Airspace CTR/TMA | BAZL | OGD swisstopo |
| Obstacles | BAZL `luftfahrthindernis` | OGD swisstopo |
| Lifts | OpenStreetMap | ODbL |
| Trails | OpenStreetMap | ODbL |

## Disclaimers

This is not a flight planner. Verify launches independently, check current
DABS / NOTAMs, respect seasonal Wildruhezone validity dates (we treat all as
year-round). The auto-discovery algorithm is not validated against verified
launch databases.

## License

MIT.
