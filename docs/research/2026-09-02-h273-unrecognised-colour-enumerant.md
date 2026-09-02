# Research memo: what an unrecognised H.273 colour enumerant means to a bag cast

2026-09-02, for #2114 (milestone 48, Python Codec Block API). Question: when a bag
carries a colour value a cast does not recognise, is the bag **refused by name** or does
the value **pass through** as an opaque unknown? Today Python passes it through and Rust
refuses, and the ticket asks which behaviour we want for both.

Evidence split **[V] verified** (primary source read at a pinned revision, or measured
here) and **[I] inferred**. Specs read as text from the ITU-T PDFs; project sources read
as raw files at the revisions named below.

## Recommendation

**Neither. Coerce an unrecognised enumerant to absent — which is what our wire already
spells "unspecified" — and say so in a log line naming the field and the value. Keep
every *shape* refusal exactly as it is.**

The fork the ticket poses assumes an unrecognised string is a value the wire legitimately
carries. It is not, and cannot be: both engine producers already narrow an unrecognised
H.273 byte to `None` before it reaches a bag [V], which is precisely what H.264 and H.265
Annex E mandate of a decoder. So spec growth cannot deliver an unknown string through any
engine path, and the forward-compatibility argument for pass-through has no mechanism
behind it. What is left is a producer bug — and refusing it at the read punishes 20 call
sites for a field none of them reads, to catch a mistake made at a seam the plan already
documents as unvalidated for every bag key.

The split is principled rather than a compromise: **the bag's shape is our contract and we
refuse violations of it; the enumerant set belongs to an external registry and we do not
own it.** A non-mapping `color_info`, a non-string member, a `bool` where an int belongs —
all still refused by name, unchanged. Only a well-formed string naming a colour we cannot
place becomes `None`, loudly.

This is a recommendation, not a standards requirement. H.273 says nothing about a
string-named tuple on an internal message; see §What the specs do not say.

## Key findings

- **H.273 contains no rule for unrecognised values at all** [V]. The word "decoder" appears
  zero times in the in-force 2024 edition; "shall" appears six times, four of them
  boilerplate. Its reserved rows constrain the *writer* — §3.6: values "shall not be used in
  identifiers conforming to this version" — and §7.2 hands out-of-range handling to "the
  specification using the code point". Where the drafters wanted a reader-side fallback they
  wrote one: §8.6 says an unrecognised sample aspect ratio "shall be considered unspecified".
  They did not write that in §8.1, §8.2 or §8.3.
- **The rule lives one layer down, and is asymmetric** [V]. H.265 §E.3.1: *"Decoders shall
  interpret reserved values of colour_primaries as equivalent to the value 2"*, stated for
  all three fields. H.264 §E.2.1 states it for `transfer_characteristics` and
  `matrix_coefficients` and — verified twice, independently, across the 08/2021 and 06/2026
  editions — **says nothing at all for `colour_primaries`**. Cite H.265 Annex E for a
  normative mapping; the H.264 primaries citation does not exist.
- **Value 2 is a permanent sentinel, not a reserved one** [V]. H.273 §3.8 defines
  *unspecified* as having no meaning "and will not have a specified meaning as an integral
  part in any future versions". H.273 defines *reserved* (§3.6) separately, and §8.3's
  catch-all calls an unrecognised matrix "reserved for future definition" — undefined,
  pointedly not unspecified. The spec proves it knows the difference.
- **The cadence is near-zero** [V]. Four editions, 2016-12-22 → 2021-07-14 → 2023-09-29 →
  2024-07-14 (in force), no amendments or corrigenda. Across 7 years 7 months: **zero**
  ColourPrimaries additions, **zero** TransferCharacteristics additions, and three
  MatrixCoefficients values (15 IPT-C2, 16 YCgCo-Re, 17 YCgCo-Ro) added in the single 2023
  edition. "The day a driver publishes a newer transfer characteristic" has not happened once
  since the first edition.
- **Our producers already conform** [V]. `v4l2_color.rs` — "any unrecognized enumerant
  propagates as `None`" — and `h273_color_vui_to_color_info`, `None` "for a value the bag
  vocabulary does not model". An unrecognised byte becomes absence, and absence is
  `ARCHITECTURE.md`'s own "every consumer treats absent as all-`unspecified`". The mapping
  H.265 mandates is the mapping we ship.
- **No media stack in the survey refuses a frame over colour** [V]. Five of six public data
  models coerce silently to a single unknown member and deliver the frame: FFmpeg
  (`h2645_vui.c:71-88`, `// Set invalid values to "unspecified"`, no warning, no error
  return), libplacebo (`PL_COLOR_PRIM_UNKNOWN`, then `pl_color_space_infer` defaults to
  BT.709/BT.1886), GStreamer (`case 2: default: return …_UNKNOWN`), WebCodecs/Blink
  (`default: // Other values map to unspecified for now` → IDL `null`), Chromium
  `media::VideoColorSpace` (`GetPrimaryID` → `INVALID`). Two of them go further and *guess*
  BT.709. The only refusal found anywhere in the survey is serde's default derive — a
  serialisation-framework default, not a media decision.
- **Every layer near the wire is open** [V]. Matroska stores an EBML `uinteger` (RFC 9559
  §5.1.4.1.28); QuickTime `colr` stores a 16-bit index. And the seam our codec path actually
  sits on — `StdVideoH264SequenceParameterSetVui` — types `colour_primaries`,
  `transfer_characteristics` and `matrix_coefficients` as bare `uint8_t` while enum-ing
  thirteen neighbouring fields in the same header, `aspect_ratio_idc` three lines above
  included. Khronos drew the line exactly at "tables an external registry keeps extending".
  GStreamer and Chromium independently converged on the same two-layer split: raw byte in the
  parser struct, coerced enum at the public API. **Where the raw value dies, it dies at a
  hand-written public model — never at the wire.**

## The tree today — three behaviours, not two

The ticket describes a two-way split. There are three [V]:

| Seam | An unrecognised colour string |
|---|---|
| Python cast (`_color_info_or_none`) | passes through — `typing.cast` is a type assertion with no runtime effect |
| Python `show()` (`python_processor_owned_window.rs:432-443`) | **refused by name** — deserialised through the same Rust enums, *"…which is not an H.273 {axis} name the bag carries"* |
| Rust cast (`video_frame.rs`, `encoded_video_frame.rs`) | refuses — serde `unknown variant` |

So pass-through is not an end-to-end contract today; it relocates the raise to the display
seam. A Python author who hand-spells `ColorInfo(primaries="bt_709")` and calls `show()`
already gets a good error — from the wheel's Rust side, not from the cast.

Two further facts about the Rust refusal, both measured here in a scratch crate against
serde [V]:

- **It kills the whole read, not the colour field.** A bag with
  `color_info.primaries = "not_a_primary"` fails the entire `VideoFrame` deserialize with
  `unknown variant`, so `surface_id`, `width` and `timestamp_ns` become unreachable too.
- **On the encoded cast it is also misreported.** `read_encoded_video_frame_bag` maps that
  failure to `NotAnEncodedVideoFrameBag`, which prints *"the bag carries no
  encoded-video-frame keys"* — untrue; all eight are present. The doc comment three lines
  above the wire struct says `codec` is held as a wire string *"so an unknown codec is
  refused naming the string rather than failing the whole decode opaquely"*. Colour is the
  one field that still does the thing that comment rejects. **This is a defect under any
  outcome of the fork.**

Blast radius, measured [V]: **20 files read `into=VideoFrame`; zero read `color_info` off a
cast** — no Python consumer in `examples/` or `packages/` touches colour at all. No test
locks the current pass-through, so a refuse ruling breaks no existing assertion; it changes
runtime behaviour only, at 20 sites, for a field none of them uses.

## Why the audio precedent does not settle it

`ARCHITECTURE.md` refuses an unknown `dtype` by name, on the doctrine that a bag is never
"reshaped into a plausible-looking wrong answer". The asymmetry is real and is the honest
reason colour can be lenient where `dtype` cannot [I]:

- Coercing an unknown `dtype` to `f32` reinterprets the sample bytes. The output is wrong
  *data*, and it looks fine.
- Coercing an unknown primaries to absent is what absent already means, on a field that
  cannot touch a pixel. Its whole reach is swapchain colourspace negotiation and encoder VUI;
  the worst case is the same default a frame carrying no `color_info` at all already gets —
  which is most frames today.

A log line naming the field and the value is what keeps this from being the ecosystem's
*silent* coercion, which the plan does ban by name. FFmpeg's is silent; ours should not be.

## What the specs do not say

- H.273 imposes nothing on any reader, so no "unknown → unspecified" claim can be sourced to
  it [V]. The H.264/H.265 mandate is about a **byte in a bitstream**, not a string on our
  wire. Our vocabulary is our own minting; the specs are the reason our *producers* narrow,
  not a requirement on our casts.
- "Map unrecognised to an explicit unspecified variant" is a design inference from that
  mandate, not spec text [I].

## Mechanical notes for whoever implements the ruling

Measured here [V]:

- `#[serde(other)]` **compiles on an externally-tagged unit-variant enum** and reads
  correctly — but serde documents it for internally- and adjacently-tagged enums only, so
  this is undocumented behaviour. It is disqualified for a wire type regardless: the catch-all
  must be a unit variant, so it discards the string and **re-serialises `"bt2111"` as
  `"Unknown"`**, silently rewriting any bag a Rust processor reads and republishes.
- An untagged `enum { Known(Primaries), Unknown(String) }` round-trips losslessly both ways
  (`"zzz"` → `Unknown("zzz")` → `"zzz"`) and still refuses a non-string. Its refusal message
  degrades to *"data did not match any variant of untagged enum"*, so a named refusal wants a
  hand-written `Deserialize`.
- `#[serde(from = "T")]` with an infallible `From` makes the deserialiser total by
  construction — it cannot refuse. `try_from` is the refusing sibling.

## Three facts to record regardless of the ruling

- The `NotAnEncodedVideoFrameBag` refusal misreports a colour failure (above).
- **Nothing gates the two vocabularies against each other.** The Python `Literal`s and the
  Rust enums are byte-identical today — 11 primaries / 16 transfer / 13 matrix / 2 range,
  diffed mechanically [V] — but there is no colour entry in `ALL_SOURCE_WALKING_GATES`. They
  agree by hand, across three hand-maintained snapshots (Rust serde enums, Python `Literal`s,
  engine ids) plus a fourth, deliberately narrower one on the escalate wire
  (`…ColorTransfer`, 5 variants).
- **The matrix table is one edition behind.** `ipt_c2`, `ycgco_re` and `ycgco_ro` appear
  nowhere in the tree [V] — exactly the three values H.273 added in 2023, in the one table
  that has ever grown. The `color_vui.rs` comment "full H.273 tables are larger" is true only
  for matrix, and only by three.

## What remains unknown

- **ISO/IEC 23091-2's own edition list.** iso.org returns 403 to every fetch; the twin-text
  relationship is primary-verified from H.273 (V4)'s Summary, but the ISO edition dates are
  not, and the ISO cadence appears offset from ITU's.
- **Pending H.273 work items.** The T-REC page lists four editions and no amendments; the
  ITU-T work-programme search did not return H.273 items, so an unpublished item cannot be
  ruled out.
- **ISO/IEC 14496-12 §12.1.5 `ColourInformationBox` field declarations** — paywalled. The
  QuickTime `colr`/`nclc` layout is verified from Apple's spec; the ISOBMFF claim rests on
  FFmpeg's `avio_rb16()` read, which is corroborating implementation evidence, not spec.
- **Chromium's and GStreamer's H.265 parser field types** — only their H.264 parsers were
  read; symmetry is assumed [I].

## What this memo does not do

It does not decide. The ruling changes `VideoFrame`'s shipped contract whichever way it
lands and applies to both casts at once, so it belongs in `/align` — and the three recorded
facts above are ordinary records, not decisions.

Sources: ITU-T H.273 (V4) 07/2024 and the 2016 / 2021 / 2023 editions, H.265 (V11) 01/2026
§E.3.1, H.264 (V16) 06/2026 and (08/2021) §E.2.1 — all `itu.int/rec/`. FFmpeg `9fc8c785`,
Vulkan-Headers `31386378`, serde `a874a1b1`, libplacebo `86bbd5df`, GStreamer `23616d5c`,
Chromium `598d07bf`. W3C WebCodecs, RFC 9559 §5.1.4.1.28, RFC 8794 §11.1.11, Apple
QuickTime File Format, MP4RA registered colour types, serde.rs variant-attrs /
container-attrs / enum-representations.
