# Legacy Desktop Prototype

This directory is retained only as historical prototype and smoke-fixture
material. It is **not** the production OpenAgent Desktop and must not receive
new product behavior.

The authoritative Desktop repository is `../../app`. Runtime behavior belongs
in OpenHarness and is exposed through the versioned Bridge manifest at
`GET /api/protocol`; React/Tauri clients only project that state.

Do not package or publish this directory. Once remaining fixture references are
removed, delete it in a dedicated cleanup change rather than evolving two
Desktop implementations.
