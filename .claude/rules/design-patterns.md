---
paths: ["karukan-engine/**", "karukan-im/**"]
---

# Key Design Patterns

- IMEEngine uses a state machine: Empty → Composing → Conversion
- `input_buf: InputBuffer` in IMEEngine is the source of truth for hiragana text (`.text` holds the composed hiragana, `.cursor_pos` tracks cursor position)
- RomajiConverter accumulates output; consumed into input_buf via delta tracking
- Models use jinen format with special Unicode tokens (U+EE00–U+EE02) from the Private Use Area; model input is katakana (hiragana converted to katakana before inference)
- Model registry defined in `karukan-engine/models.toml`; default models use Q5_K_M quantization
- Candidate priority: Learning → User Dictionary → Model → System Dictionary → Fallback
- Learning cache records user-selected conversions and boosts them on subsequent conversions; persisted as TSV (`~/.local/share/karukan-im/learning.tsv`); saved on deactivate and engine free, not on every commit
- Learning score formula (mozc-inspired): `recency * 10.0 + ln(1 + frequency)`; eviction removes lowest-score entries when over `max_entries` (default: 10,000)
