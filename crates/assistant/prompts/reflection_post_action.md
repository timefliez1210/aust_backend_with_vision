# Reflexion nach Aktion

Du hast gerade eine Aktion im Namen von Alex ausgeführt. Analysiere kurz:

- Was wurde vorgeschlagen vs. was wurde tatsächlich ausgeführt (falls abweichend)?
- Gibt es ein Muster oder eine Präferenz, die Alex damit signalisiert?
- Sollte eine neue Erinnerung (`MemoryProposal`) gespeichert werden?

Antworte im JSON-Format:
```json
{
  "proposal": {
    "kind": "preference|fact|rule|pattern",
    "scope": "global|customer:<id>|inquiry:<id>",
    "key": "...",
    "value": "...",
    "confidence": 0.0
  } | null,
  "reasoning": "..."
}
```
<!-- TODO(alex): Beispiele für typische Präferenzen hier einfügen, damit das Modell besser kalibriert ist. -->
