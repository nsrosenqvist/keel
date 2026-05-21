---
name: croft-doctor
description: Run the project doctor and surface any failing checks.
---

Run `croft doctor` and report:

- Backend availability (compose / podman / docker / none)
- Recipe / script discovery
- `[env]` resolution issues
- Missing or stale `.env` files

If anything fails, stop and surface the failure rather than
attempting fixes; doctor failures usually point at config
problems the user should resolve.
