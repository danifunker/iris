# Pipeline backup

Snapshot of all pipeline and deployment files created for the `build-pipeline-danifunker` branch.
To restore any file, copy it back to its original location:

```
backup/pipeline/.github/workflows/release.yml       → .github/workflows/release.yml
backup/pipeline/.github/workflows/sync-upstream.yml → .github/workflows/sync-upstream.yml
backup/pipeline/.github/workflows/appstore.yml      → .github/workflows/appstore.yml
backup/pipeline/installer/iris-gui.iss               → installer/iris-gui.iss
backup/pipeline/installer/iris-gui.entitlements      → installer/iris-gui.entitlements
backup/pipeline/installer/iris-gui-notarized.entitlements → installer/iris-gui-notarized.entitlements
backup/pipeline/iris-gui/iris-gui.desktop            → iris-gui/iris-gui.desktop
backup/pipeline/docs/appstore-build.md               → docs/appstore-build.md
backup/pipeline/docs/handoff-pipeline.md             → docs/handoff-pipeline.md
```

Bulk restore (run from repo root):

```bash
cp backup/pipeline/.github/workflows/*.yml .github/workflows/
cp backup/pipeline/installer/* installer/
cp backup/pipeline/iris-gui/iris-gui.desktop iris-gui/
cp backup/pipeline/docs/appstore-build.md backup/pipeline/docs/handoff-pipeline.md docs/
```

**Keep this backup in sync** whenever you update any of the above files:

```bash
cp .github/workflows/release.yml .github/workflows/sync-upstream.yml .github/workflows/appstore.yml backup/pipeline/.github/workflows/
cp installer/iris-gui.iss installer/iris-gui.entitlements installer/iris-gui-notarized.entitlements backup/pipeline/installer/
cp iris-gui/iris-gui.desktop backup/pipeline/iris-gui/
cp docs/appstore-build.md docs/handoff-pipeline.md backup/pipeline/docs/
```
