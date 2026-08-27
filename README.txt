KUZ NOR v0.1.0 Alpha
=====================

Kuznor es un asistente de IA local, privado y open source.

Funciones principales:
- General: chat con modelos locales.
- Documentos: RAG local con bibliotecas y fuentes.
- Código: análisis read-only de archivos y proyectos.

IMPORTANTE
----------
Kuznor no incluye modelos GGUF ni los descarga automáticamente.

Para usarlo necesitas:
1. Un modelo principal GGUF compatible con llama.cpp.
2. Un modelo local de embeddings compatible.
3. Un backend llama.cpp funcional.

La calidad de las respuestas depende del modelo local seleccionado.

Kuznor puede cometer errores o generar información incorrecta.
Para información importante, verifica siempre las fuentes originales.

PRIVACIDAD
----------
Kuznor está diseñado para funcionar localmente.

v0.1.0 Alpha:
- Sin cuenta.
- Sin telemetría.
- Sin APIs externas para la IA.
- Sin búsquedas web.
- Código en modo read-only.
- SQLite local.

NOTA:
La base de datos SQLite todavía no está cifrada en reposo.

WINDOWS SMARTSCREEN
-------------------
Esta versión Alpha no está firmada digitalmente.

Windows SmartScreen puede mostrar:
"Windows protegió su PC" / "Editor desconocido".

Si descargaste Kuznor desde el GitHub oficial de Inference Lab,
verifica el hash SHA-256 publicado en el release antes de ejecutarlo.

ARCHIVOS
--------
kuznor.exe
LICENSE
THIRD_PARTY_LICENSES.md
README.txt

LICENCIA
--------
Kuznor se distribuye bajo GPL-3.0-only.

Las dependencias, llama.cpp y los modelos de IA conservan sus
respectivas licencias.

FILOSOFÍA
---------
Kuznor lee, analiza y propone. El usuario decide.

Local intelligence, forged in Rust.