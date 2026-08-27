# Kuznor

**Local intelligence, forged in Rust.**

Kuznor is a **100% local, private, free and open-source AI assistant built in Rust**.

It is designed for people who want to work with local AI models while keeping their documents, source code and conversations under their own control.

> **Kuznor lee, analiza y propone. El usuario decide.**

> **Private by design. Assistive by purpose. User-controlled by default.**

---

## Status

**Current release:** `v0.1.0-alpha`

Kuznor is currently in **Alpha**. The core workflows are functional, but bugs, UI issues and model-dependent inaccuracies may still occur.

This release is intended for early testing, technical feedback and validation with real users.

---

## What Kuznor does

Kuznor currently provides three main modes:

### General

A local AI chat for:

- Questions and explanations
- Summaries
- Ideas
- Basic reasoning
- General programming questions
- Local conversations without sending prompts to cloud AI services

The quality of the response depends heavily on the local model selected by the user.

### Documents

A local document-analysis workspace based on **RAG**.

Kuznor can:

- Create and manage document libraries
- Index supported local documents
- Search relevant fragments using local embeddings
- Summarize documents
- Compare several documents
- Answer questions using the selected library
- Show the files/pages used as sources
- Keep libraries isolated from each other

Current supported document formats in `v0.1.0-alpha`:

- PDF with extractable text
- TXT
- Markdown

> Scanned PDFs without extractable text are not yet supported through OCR.

### Code

A **read-only** local code-analysis mode.

Kuznor can:

- Open individual source files
- Open and index complete projects
- Explain the selected file
- Analyze relationships inside the active project
- Detect possible logical issues
- Suggest improvements and refactors
- Summarize code
- Search relevant code fragments
- Show the source files used for the answer
- Remove a project from Kuznor without deleting the original files

Kuznor **does not execute, compile, install, modify or delete project source code** in this release.

---

## Privacy

Kuznor is designed around local processing.

In `v0.1.0-alpha`:

- No cloud AI is required
- No user account is required
- No telemetry is sent
- No analytics are sent
- No external AI APIs are required
- No web search is performed
- No automatic model download is performed
- Chat inference runs through local `llama.cpp`
- Embeddings are generated locally
- Documents and projects remain on the user's machine
- Network endpoints are limited to local loopback usage in this release

Kuznor intentionally avoids indexing common sensitive files such as environment files, credentials, keys and certificates in Code mode.

### Important storage note

Kuznor currently uses local SQLite storage.

**SQLite data is not encrypted at rest in `v0.1.0-alpha`.**

This means indexed text fragments and local history may be readable by someone who already has access to the user's machine and Kuznor data files.

Encryption and broader security hardening are planned for future releases.

---

## Architecture

Kuznor is not the language model itself.

It acts as a local orchestration layer between the user and compatible local AI models.

Main components:

- **Rust**
- **egui / eframe**
- **SQLite / rusqlite**
- **reqwest**
- **serde / serde_json**
- **llama.cpp**
- **GGUF chat model**
- **GGUF embedding model**
- Local RAG pipeline
- Local source indexing

Simplified flow:

```text
User
  ↓
Kuznor
  ├─ General ───────────────→ Local chat model
  ├─ Documents → RAG ──────→ Local chat model
  └─ Code → Project index ─→ Local chat model
                               ↑
                         llama.cpp
```

---

## Requirements

### Operating system

`v0.1.0-alpha` is currently focused on:

- **Windows 10 / Windows 11 x64**

Linux support is part of the long-term direction, but this Alpha should currently be considered Windows-first.

### Hardware

Actual requirements depend primarily on the GGUF model selected.

Recommended baseline for comfortable local use:

- 64-bit CPU
- 16 GB RAM
- Modern multi-core processor
- SSD storage
- Approximately 3–10+ GB of free space depending on downloaded models

A dedicated GPU is **not required**, but compatible hardware acceleration can improve inference speed substantially.

Small 4B-class models are practical on many modern systems, but larger models require more RAM/VRAM.

---

## llama.cpp

Kuznor uses `llama.cpp` as the local inference backend.

The user is responsible for providing a compatible `llama.cpp` installation/binary and compatible local GGUF models.

Kuznor manages the local interaction with the configured model, but the external `llama.cpp` binary remains third-party software outside Kuznor's direct control.

---

## Models

Kuznor is intended to remain **model-agnostic**.

The user chooses the local GGUF model.

Models tested during development include:

### Chat / main model

- Gemma 3 4B Instruct Q4_K_M
- Qwen3 4B Q4_K_M
- Qwen3 1.7B Q4_K_M

### Embeddings

- nomic-embed-text-v1.5 Q4_K_M

These are examples, not mandatory dependencies.

Different models may vary significantly in:

- Reasoning
- Accuracy
- Language quality
- Instruction following
- Structured output
- Hallucination rate
- Speed
- RAM / VRAM requirements

> **The quality of Kuznor's answers depends on the local model selected.**

Kuznor does not automatically rewrite or moderate the personality/style of the selected model. Application-level permissions remain controlled by Kuznor.

---

## Installation

### Option A — Portable Windows build

For users who only want to test Kuznor:

1. Download the latest Windows x64 portable release.
2. Extract the ZIP.
3. Run `Kuznor.exe`.
4. Configure:
   - Main GGUF model
   - Embedding GGUF model
5. Make sure the required local `llama.cpp` backend is available/configured.
6. Start using General, Documents or Code.

> The exact portable-package layout may change during the Alpha period.

### Option B — Build from source

Requirements:

- Rust toolchain
- Cargo
- Git
- A compatible `llama.cpp` setup

Clone the repository:

```bash
git clone <REPOSITORY_URL>
cd kuznor
```

Development build:

```bash
cargo build
```

Run:

```bash
cargo run
```

Release build:

```bash
cargo build --release
```

Validation:

```bash
cargo fmt --check
cargo check
cargo test
```

---

## Basic usage

### General

1. Open Kuznor.
2. Select **General**.
3. Create or open a chat.
4. Ask a question.
5. Kuznor sends the request to the configured local model.

### Documents

1. Open **Libraries and documents**.
2. Create a library.
3. Add supported documents.
4. Wait for indexing to complete.
5. Open **Documents** mode.
6. Select the desired library.
7. Ask questions about its contents.
8. Review the sources shown under important responses.

Libraries are independent from chats. A chat can use the currently selected library as context.

### Code

1. Open **Code**.
2. Choose a source file or project folder.
3. Wait for indexing if a project was selected.
4. Select the file/project context.
5. Ask Kuznor to explain, review or analyze it.

Code mode is read-only in this release.

---

## Sources and verification

Kuznor attempts to keep source context traceable in Documents and Code.

However:

- Retrieval can miss relevant information
- PDF text extraction may be imperfect
- Small models can misunderstand context
- Models can hallucinate
- Summaries and interpretations are not guaranteed to be correct

Kuznor is an **assistive tool, not a substitute for user judgment**.

For important information:

> **Always verify the response against the original sources.**

---

## Known limitations — v0.1.0 Alpha

Current known limitations include:

- Alpha-quality UI and UX
- Model quality varies significantly
- Local models may hallucinate
- Document extraction quality depends on the source file
- Scanned/image-only PDFs are not supported through OCR yet
- DOCX/XLSX/PPTX support is not included yet
- No image/VLM analysis yet
- No LAN Compute yet
- No web access
- No agents
- No terminal integration
- No execution of generated code
- Code mode is read-only
- No advanced AST/LSP analysis yet
- SQLite is not encrypted at rest
- No automatic GGUF downloads
- No automatic model routing
- Local inference may consume significant CPU/RAM
- External `llama.cpp` failures can affect the experience
- Larger document libraries may require more indexing and inference time

---

## What Kuznor is not

Kuznor is not intended to:

- Replace professional judgment
- Guarantee factual accuracy
- Replace an IDE
- Automatically modify source code
- Execute actions without user control
- Replace commercial AI services in every use case

Kuznor exists for situations where users **prefer or require local AI execution and direct control over their data**.

Users may continue using commercial AI tools alongside Kuznor.

---

## Roadmap

The roadmap is directional and may change based on user feedback.

### v1.0

- Feedback-driven stabilization
- Basic security improvements
- Better `llama.cpp` lifecycle/error handling
- RAG improvements
- **Experimental LAN Compute**

### v1.1

- Multiple model management
- Model profiles
- Preferred model by mode

### v1.2

- DOCX
- XLSX
- PPTX

### v1.3

- Code-project reference documentation
- Persistent Code chat/project context

### v1.4

- General improvements
- Performance
- Basic security consolidation

### v1.5

- Local OCR
- Local image/VLM support in General
- Images processed temporarily without intentional SQLite storage

### v1.6

- Technical project memory
- User-controlled persistent decisions and conventions

### v1.7

- Advanced local technical documentation
- Version-aware documentation
- Hybrid semantic + exact retrieval

### v1.8

- Deeper Code understanding
- Symbols
- Imports
- Dependencies
- Call relationships
- Architecture analysis
- Impact analysis

### v1.9

- Full-app consolidation
- Performance
- UX
- Stability
- Security refinements

### v2.0

**Kuznor Security Hardening**

Planned direction:

- Formal threat model
- Optional encryption at rest
- Stronger secret handling
- Parser hardening
- LAN authentication and encrypted transport
- Integrity verification
- Signing
- Local security auditing
- Security regression suite
- Residual-risk documentation

---

## Screenshots

> Replace these paths with real repository screenshots before publishing.

### General

```text
docs/screenshots/general.png
```

### Documents

```text
docs/screenshots/documents.png
```

### Code

```text
docs/screenshots/code.png
```

### Settings

```text
docs/screenshots/settings.png
```

Recommended Markdown once screenshots are added:

```md
![Kuznor General](docs/screenshots/general.png)
![Kuznor Documents](docs/screenshots/documents.png)
![Kuznor Code](docs/screenshots/code.png)
```

---

## Contributing

Kuznor is currently in an early Alpha stage.

Feedback is especially useful in:

- Rust architecture
- egui / eframe UX
- `llama.cpp` process management
- Local RAG quality
- Document extraction
- Model compatibility
- Performance
- Security
- Windows packaging
- Bug reports and reproducible edge cases

Contribution guidelines will be expanded as the project matures.

---

## License

Kuznor is intended to be released as **free and open-source software under the GNU General Public License v3.0 (GPLv3)**.

See:

```text
LICENSE
```

for the complete license terms.

Third-party components and models retain their respective licenses.

---

## Disclaimer

Kuznor is provided without any guarantee that model-generated output is correct, complete or suitable for a particular purpose.

Local AI models can produce incorrect or fabricated information.

The user remains responsible for:

- Verifying important outputs
- Selecting appropriate models
- Reviewing model licenses
- Controlling access to local data
- Confirming actions taken based on generated suggestions

Kuznor does not automatically execute generated instructions in `v0.1.0-alpha`.

---

## Philosophy

> **Kuznor lee, analiza y propone. El usuario decide.**

**Local intelligence, forged in Rust.**

**Nothing leaves your machine. Nothing changes without you.**
