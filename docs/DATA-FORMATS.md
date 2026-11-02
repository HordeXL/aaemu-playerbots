# Baked data formats

Written by `aa-bake` (`playerbots bake`), read by `aa-world`. All integers little-endian.
World coordinates: metres, x east, y north, z up; `main_world` spans 35×38 cells of 1024 m
(0..35840 × 0..38912), sea level 100 m.

## `heights_<world>.bin` — terrain height grid

| Offset | Type | Field |
| --- | --- | --- |
| 0 | `[u8; 8]` | magic `PBHGT001` |
| 8 | u32 | `width` — samples along x |
| 12 | u32 | `height` — samples along y |
| 16 | f32 | `origin_x` — world x of sample column 0 |
| 20 | f32 | `origin_y` — world y of sample row 0 |
| 24 | f32 | `step` — metres between samples (default 4.0; the server's native grid is 2.0) |
| 28 | f32 | `scale` — metres per raw unit (1/64, same quantisation as the server) |
| 32 | f32 | `offset` — metres added after scaling (0.0) |
| 36 | u32 | `flags` — bit 0: cells without terrain hold raw 0 |
| 40 | `[u8; 24]` | reserved, zero |
| 64 | `[u16; width*height]` | samples, row-major: `index = row * width + col`, row = y, col = x |

Height at (x, y): `fx = (x - origin_x) / step`, `fy = (y - origin_y) / step`, bilinear blend of
the 4 surrounding samples, `h = raw * scale + offset`. Outside the grid: `None`.
`aa-world` memory-maps the file, so only the pages around bots become resident.

## `map_<world>.png` + `map_<world>.json` — world map image and calibration

```json
{
  "image": "map_main_world.png",
  "width": 1856,
  "height": 1112,
  "source": "painted",            // "painted" (client ui/map DDS) or "relief" (from heights)
  "world": { "min_x": -14731.0, "min_y": 46.0, "max_x": 49920.0, "max_y": 38911.0 }
}
```

Pixel (0,0) is the top-left corner = world (`min_x`, `max_y`).
`u = (x - min_x) / (max_x - min_x)`, `v = 1 - (y - min_y) / (max_y - min_y)`, pixel = (u·width, v·height).

## `housing_areas.json` — housing area polygons

```json
[ { "id": 52, "zone_key": 179, "points": [[13900.0, 14180.0], [13990.0, 14180.0], ...] } ]
```

`id` is `housing_areas.id` in compact.sqlite3 (`value1` in the client's `housing_area.xml`);
points are world x/y. `aa-world` joins group/category rules from compact.

## `quest_spheres.json` — quest sign spheres

```json
[ { "quest": 1234, "component": 5678, "x": 15000.0, "y": 15000.0, "z": 120.0, "radius": 10.0 } ]
```

From `level_design/zone/<zone_key>/client/quest_sign_sphere.g`; world position =
zone origin cell × 1024 + local position.

## `zones_<world>.bin` — zone key per 64 m sector (optional)

| Offset | Type | Field |
| --- | --- | --- |
| 0 | `[u8; 8]` | magic `PBZON001` |
| 8 | u32 | `width` (sectors along x) |
| 12 | u32 | `height` (sectors along y) |
| 16 | f32 | `sector` — metres per sector (64.0) |
| 20 | `[u8; 12]` | reserved |
| 32 | `[u16; width*height]` | zone key, row-major (`row * width + col`), 0 = none |
