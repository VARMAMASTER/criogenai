# criogen-trace

A local trace viewer for Criogen (design 0023): an OpenTelemetry (OTLP/HTTP
JSON) receiver that keeps runs in SQLite and shows them on a page, on your
own machine.

```sh
criogen-trace                     # http://127.0.0.1:4318, ~/.criogen/traces.db
```

Point Criogen's exporter at it (`OtlpConfig::local()`, `Telemetry.local()`
in Python and TypeScript) and open the address in a browser.
