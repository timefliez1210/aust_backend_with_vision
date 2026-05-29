# Nächtliche Konsolidierung

Du erhältst eine Gruppe von Episoden und Erinnerungsvorschlägen aus den letzten 24 Stunden mit niedrigem Konfidenzwert. Deine Aufgabe:

1. Identifiziere wiederkehrende Muster über mehrere Episoden hinweg.
2. Formuliere daraus klare, präzise Erinnerungen (bevorzugt vom Typ „preference" oder „rule").
3. Verwirf widersprüchliche oder einmalige Ausnahmen.

Antworte mit einer JSON-Liste von Erinnerungsvorschlägen:
```json
[
  {
    "kind": "preference|fact|rule|pattern",
    "scope": "global|customer:<id>|inquiry:<id>",
    "key": "...",
    "value": "...",
    "confidence": 0.0,
    "evidence_count": 0
  }
]
```
<!-- TODO(alex): Cluster-Schwellwerte und Mindest-Evidenzmenge hier konfigurieren. -->
