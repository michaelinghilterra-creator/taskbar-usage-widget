# Taskbar Usage Widget: visual spec (concept 2a)

Concept: **Forecast strip + Headroom card**. Mockups: `Taskbar Widget Concepts.dc.html`, sections 2a (strip + card, dark and light) and 3a (states).

All sizes are at **100% scale** (96 DPI). Multiply by the DPI scale (1.25, 1.5, 1.75, 2.0). Round rect positions and sizes to whole device pixels. Strokes: `max(1, round(w × scale))` device px, except antialiased arcs and curves, which can stay fractional.

Fonts: **Segoe UI Variable Text** for text up to 14 px, **Segoe UI Variable Display** for 20 px and up. Fall back to Segoe UI. Digits in Segoe UI are tabular by default, so numbers do not jitter when they change.

---

## 1. Shared visual language

The strip and the card use the same meter vocabulary. Every meter is built from these layers, back to front:

| Layer | Meaning | Range |
|---|---|---|
| Track | Whole window | 0 → 100% |
| Elapsed | Time already passed in the window | 0 → elapsed% |
| Projection (card only) | Where usage is forecast to land at reset | used% → projected% |
| Fill | Usage | 0 → used% |

Reading rule: fill that stays inside the lighter elapsed area is under pace. Fill that goes past it is ahead of pace. The strip has no pace tick; the edge of the elapsed layer does that job.

`projected% = min(100, used% / elapsed% × 100)` (or your existing forecast value, if it is smarter).

### State colors (fill and number)

| State | Rule (unchanged from today) | Fill | Number |
|---|---|---|---|
| ok | default | provider accent | Text |
| warn | > 10 points ahead of pace and past 50% | Warn | Warn |
| crit | forecast runs out before reset, or > 95% | Crit | Crit |

---

## 2. Colors

### Accents (both themes)

| Role | Hex |
|---|---|
| Claude | `#D97757` |
| Codex | `#4C9AFF` |
| Spark | `#76B900` |

### Taskbar strip

| Role | Dark | Light |
|---|---|---|
| Taskbar behind strip (reference only) | `#1C1C1C` | `#F3F3F3` |
| Number text | `#F2F2F4` | `#1B1B1F` |
| Muted (labels, units, idle) | `#9A9AA3` | `#5F5F68` |
| Track | `#3A3A40` | `#D4D4DA` |
| Elapsed | `#5A5A62` | `#B0B0B8` |
| Divider | `#3A3A40` | `#D0D0D6` |
| Warn | `#E8A33D` | `#A86200` |
| Crit | `#FF6B61` | `#C42B1C` |

### Card

| Role | Dark | Light |
|---|---|---|
| Card background | `#1E1E21` | `#F3F3F6` |
| Card border | `#323238` | `#DCDCE1` |
| Panel fill | `#26262B` | `#FFFFFF` |
| Panel border | `#2E2E33` | `#E4E4E9` |
| Text | `#EDEDF0` | `#1B1B1F` |
| Muted | `#9A9AA3` | `#5F5F68` |
| Faint (resets, axis labels, sub-values) | `#6E6E77` | `#85858E` |
| Meter track | `#36363C` | `#E4E4E9` |
| Meter elapsed | `#4C4C54` | `#C8C8CF` |
| Separator inside panels | `#34343A` | `#E8E8EC` |
| Service chip fill | `#1E1E21` | `#F3F3F6` |
| Good (under-pace text) | `#7EAE5B` | `#41781C` |
| Warn | `#E8A33D` | `#A86200` |
| Crit | `#FF6B61` | `#C42B1C` |
| Idle dot | `#8A8A93` | `#8A8A93` |
| Projection segments | state fill at alpha 0x55 (33%) | same |
| Drop shadow (optional) | 0, 8, blur 32, black 50% | 0, 8, blur 32, black 16% |

### Status pill fills (card)

| Pill state | Dark | Light | Dot |
|---|---|---|---|
| good | `#7EAE5B` at 14% | `#41781C` at 10% | Good |
| crit | `#FF6B61` at 16% | `#C42B1C` at 10% | Crit |
| warn | `#E8A33D` at 16% | `#A86200` at 10% | Warn |
| stale / idle | `#2C2C31` | `#E6E6EA` | Idle dot |
| spark busy | `#76B900` at 14% | `#76B900` at 14% | Spark |

---

## 3. Taskbar strip

Height 48. Transparent background. All content is vertically centered.

```
[pad 4][Claude group][8][div][8][Codex group][8][div][8][Spark group][pad 4]
```

- **Divider:** 1 × 20, Divider color, vertically centered.
- **Width with all groups:** about 325 px (today: about 360).

### Limit group (Claude, Codex)

```
[mark 16][8][item][8][item][8][item]
```

- **Mark:** 16 × 16, vertically centered (geometry in section 6).
- **Item:** 30 wide, 22 tall (15 text + 4 gap + 3 bar), centered vertically (top = 13).
  - **Text row (15 tall), baseline-aligned:**
    - Key `5h` / `W` / `F` on the left: 10 regular, Muted.
    - Number on the right, **no % sign** (`3`, `25`, `100`): 13 semibold, line height 15, state color.
  - **Bar:** 30 × 3, radius 1.5, layered Track → Elapsed → Fill. Fill uses the state color.
- **Codex:** one item today (`W`). Add `5h` automatically when the plan has it.

### Spark group

```
[chip 16][6][value][3][unit][6][sparkline 36×18]
```

- **Value:** 13 semibold.
  - Busy: `126`, Text color.
  - Idle: `idle`, Muted.
  - Down: `down`, Crit.
- **Unit:** `t/s`, 10 regular, Muted. Hidden when idle or down.
- **Chip color:** Spark green when busy, Muted when idle, Crit when down.
- **Sparkline:** 36 × 18, last 10 min.
  - Line: 1.25 px, Spark green.
  - Area: vertical gradient from 38% to 2% alpha.
  - Vertical scale: y = 17 − (v / max) × 14.
  - Newest point: dot r 1.75, plus a halo r 3.85 at 22% alpha that pulses (section 7).
  - Idle or down: flat 1.25 px line in Track color, no area, no dot.

### Vertical taskbar

- Stack the groups top to bottom with 8 gap. The divider becomes 20 × 1.
- Inside a limit group, the mark sits on its own row (centered), then the items stack with 6 gap.
- Item and Spark-group geometry stays the same; the sparkline sits under the value.

---

## 4. Strip states

| State | Treatment |
|---|---|
| Normal | As above |
| Warning | Number and fill in Warn |
| Critical | Number and fill in Crit. On entering critical, the number alpha-pulses 3 times (section 7), then holds steady. |
| Stale | Whole group drawn at 45% opacity, values kept. Codex: stale when the reading is older than 1 h. |
| Loading | Number shows `··` in Muted. Bars show the track only, pulsing 40–100% opacity on a 1.2 s cycle. Stops on first data. |
| Empty / error, nothing cached | Number shows `–` in Muted. Bars show the track only. No animation. |
| Error with cached values | Use Stale. |
| Spark busy / idle / down | See Spark group above |

---

## 5. Card

Width 360. Height grows with content.

- **Card:** radius 10, 1 px border, padding 12. Vertical gap between blocks: 8.
- Opens above the strip, right edge aligned with the strip's right edge, 12 px gap above the taskbar.

Inner widths:

- Card content: 360 − 2 (border) − 24 (padding) = **334**.
- Panel content: 334 − 2 (border) − 24 (padding) = **308**.

### 5.1 Header row

- Padding: 4 top, 4 left and right, 2 bottom.
- `Usage`: 14 semibold, Text, left.
- Right side, 10 gap between items:
  - `24 s ago`: 11 regular, Muted.
  - Refresh, pin and close glyphs, each 14 × 14, Muted (geometry in section 6).

### 5.2 Status pills (the headline)

- Row padding: 0 left/right 4, bottom 4. Gap 6 between pills; wrap if needed.
- **Pill:** height 21, fully rounded (radius 10.5), padding 7 left / 9 right. Fill from section 2.
  - Dot: 6 × 6 circle, 6 gap to the text.
  - Text: 11 regular, Text color.
- Text pattern: `Claude · On pace`, `Claude · Runs out 8:52 PM`, `Codex · Stale · 6 h`, `Spark · 126 t/s`, `Spark · Idle`, `Spark · Down`.
- Each pill reflects the worst limit of its provider.

### 5.3 Provider panel (Claude, Codex, Spark)

- **Panel:** radius 8, 1 px Panel border, Panel fill, padding 12. Vertical gap between rows: 12.
- **Panel header:**
  - Mark 14 × 14, 7 gap, then the name in 13 semibold, Text.
  - Right side: meta text in 10.5 regular, Muted.
    - Claude: `271.4 M tokens today`
    - Codex: `Plus · 30.7 M today · 5 m ago`
    - Spark: model name
  - Clicking the header opens the provider page.

### 5.4 Limit block (inside the Claude and Codex panels)

Vertical gap 6 between its three rows.

1. **Numbers row, baseline-aligned:**
   - Big number `25%`: 20 semibold Display, line height 22, state color.
   - 6 gap, then `Weekly · 75% left`: 11 regular, Muted. This takes the remaining width.
   - Right: `resets Thu 8:59 AM`: 10.5 regular, Faint.
2. **Segment meter:** 20 segments across 308 px, segment height 6, gap 2, radius 1.5.
   - Segment width = (308 − 38) / 20 = 13.5. Alternate 13 and 14, or snap per device pixel.
   - Color each segment `i` (0–19) using its midpoint `m = (i + 0.5) × 5`:
     - `m < used` → state fill
     - else `m < projected` → state fill at 33% alpha
     - else `m < elapsed` → Meter elapsed
     - else → Meter track
3. **Forecast line:** 10.5 regular.
   - Good color: `▼ 24 under pace · ends near 52%`
   - Warn or Crit color: `▲ 48 ahead of pace · empty ~8:52 PM`

Optional setting: "Card number: used | left". In "left" mode the big number becomes `75%` and the label becomes `left · Weekly`. The default is "used", so the card matches the strip.

### 5.5 Claude 24 h chart (bottom of the Claude panel)

- 1 px Separator above it, then 9 padding.
- Axis label row: `5-hour · last 24 h` on the left and `now` on the right, both 10 regular, Faint. 4 gap below.
- **Chart:** 308 × 30, y range 0–100%.
  - Gridlines, 1 px Separator color, dashed 2 on / 3 off:
    - horizontal at 50% and 100%
    - vertical at 6 h, 12 h and 18 h
  - Solid 1 px baseline.
  - Area: Claude accent, gradient from 30% to 2% alpha.
  - Line: 1.5 px, round joins.
  - Newest point: dot r 2.

### 5.6 Spark panel

- **Hero row** (bottom-aligned, 12 gap):
  - Left column:
    - Value `126`: 30 semibold Display, line height 32. Text color; Crit and the word `down` when unreachable.
    - Below it: `tok/s · peak 126` in 11 regular, Muted. When down: `unreachable · 4 m`.
  - Right: the chart fills the remaining width (about 220) and is 38 tall.
    - Spark line 1.5 px; area gradient from 38% to 2% alpha.
    - Newest point: dot r 2 plus a pulsing halo r 4.4.
    - When down: Faint color, no dot.
- **Stats row:** 1 px Separator above, then 9 padding. 4 equal columns, 8 gap.
  - Label: 10 regular, Muted.
  - Value: 13 semibold, Text.
  - Sub-value: 9.5 regular, Faint.
  - Columns: `KV cache 2%` · `TTFT 495 ms` · `GPU 96%` / `77 °C · 44 W` · `Memory 51%` / `swap 54%`.
  - Running / waiting: add as a 5th column if it fits at 9.5 sub size, or move it into the hero sub line (`tok/s · peak 126 · 1 running`).
  - When down: values show an em dash glyph (U+2014) in Faint, sub-values hidden.
- **Service chips:** gap 6.
  - Chip: height 19, radius 9.5, padding 2 top/bottom and 9 left/right, Service chip fill.
  - Dot: 6 × 6, 5 gap to the text.
  - Text: 11 regular, Muted, e.g. `Wan idle`.
  - Dot color: Spark when busy, Idle dot when idle, Crit when down.

### 5.7 Card states

| State | Treatment |
|---|---|
| Stale (Codex) | Whole panel at 50% opacity. Meta shows `6 h old`. Pill: `Codex · Stale · 6 h`. |
| Loading | Skeleton blocks in Meter track color, pulsing 40–100% / 1.2 s. Per limit: a 40 × 18 block (radius 4), a 90 × 8 block (radius 4), then one full-width 6 px bar (radius 1.5). Header meta: `Loading` in Faint. |
| Empty | Centered in the panel body, padding 10 top / 6 bottom. Title `No readings yet` in 12 semibold. Below it `The first reading appears here after the next refresh.` in 11 regular, Muted. |
| Error, values cached | Under the header: warning triangle (12 px, Warn) + 7 gap + `Can't reach claude.ai · showing 10:31 PM values` in 11 regular, Muted. Limit blocks at 50% opacity. |
| Spark down | Chip and hero value in Crit. Stats show the em dash glyph (U+2014). Service chips switch to `down` with Crit dots. Pill: `Spark · Down`. |

---

## 6. Icon geometry

All glyphs are drawn on an `s × s` box (16 in the strip, 14 in the card).

- **Claude starburst:** 12 rays at 30° steps starting at 0°. Each ray runs from radius 0.10s to 0.47s (even rays) or 0.36s (odd rays). Stroke max(1.4, 0.10s), round caps. Claude accent.
- **Codex mark:** hexagon with vertices at radius 0.43s, angles 30° + k × 60° (pointy top and bottom). Stroke max(1.3, 0.085s), round joins, no fill. Inside:
  - Chevron polyline (0.33s, 0.38s) → (0.45s, 0.50s) → (0.33s, 0.62s).
  - Underscore line from (0.52s, 0.63s) to (0.67s, 0.63s).
  - Same stroke, round caps.
- **Spark chip:**
  - Outlined square inset 0.24s on each side, radius 0.08s, stroke max(1.3, 0.09s).
  - Filled center square 0.18s wide at (0.41s, 0.41s), radius 0.03s.
  - Pins: two per side, at 0.38s and 0.62s along the side. Each runs from 0.06s from the outer edge to the square, same stroke, round caps.
- **Refresh (14):**
  - Arc centered (7, 7), r 4.5, from about 135° (lower left) clockwise round to 45° (upper right), ending at (9.9, 3.55).
  - Arrowhead polyline (10.2, 1.4) → (10.2, 3.9) → (7.7, 3.9).
  - Stroke 1.3, round.
- **Pin (14):** circle center (7, 5) r 2.8, outline, plus a line from (7, 7.8) to (7, 12.5). Pinned: fill the circle.
- **Close (14):** lines (3.5, 3.5)–(10.5, 10.5) and (10.5, 3.5)–(3.5, 10.5), stroke 1.3, round caps.
- **Warning triangle (12):** path (6, 1.5) → (11, 10.5) → (1, 10.5), closed, stroke 1.3, round joins. Line (6, 5)–(6, 7.3) plus a dot at (6, 9) r 0.75. Warn color.
- **Status dot:** 6 × 6 circle.

---

## 7. Motion

Nothing animates while idle except the two loops marked "Loop" below.

| What | Timing |
|---|---|
| Spark newest-point halo (strip and card) | Loop only while Spark is busy: alpha 1 → 0.3 → 1 over 1.0 s, ease-in-out. Stop when idle or down. |
| Loading skeleton / tracks | Loop only while loading: opacity 1 → 0.4 → 1 over 1.2 s, ease-in-out. |
| Critical entry (strip number) | 3 alpha pulses (1 → 0.35 → 1, 600 ms each), then steady. |
| Number change | Roll or crossfade over 250 ms, ease-out cubic. |
| Meter fill change | Fill and segments ease to the new value over 300 ms, ease-out cubic. |
| Card open | Fade 0 → 1 plus slide up 8 px over 150 ms, ease-out. Close: fade over 100 ms. |
| Hover open delay / close delay | 350 ms / 400 ms (unchanged) |

---

## 8. Copy rules

- Strip numbers have no % sign. Card numbers always have %.
- Times: `10:19 PM` when the reset is today. Otherwise add the weekday: `Thu 8:59 AM`.
- Ages: `24 s ago`, `5 m ago`, `6 h old` (use "old" once stale).
- Forecast:
  - Under pace: `▼ {n} under pace · ends near {p}%`
  - Ahead of pace: `▲ {n} ahead of pace · ends near {p}%`
  - Will run out: `▲ {n} ahead of pace · empty ~{time}`
