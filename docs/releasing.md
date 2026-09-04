# Releasing Morons

This is the maintainer procedure for producing the six target archives defined by [ADR 0007](adr/0007-supported-processor-architectures.md). The workflow creates candidate artifacts or an unpublished GitHub draft. It never publishes a release automatically.

## Preconditions

1. The release commit is on `main`, the checkout is clean, and all required CI/security checks pass.
2. `Cargo.toml` contains the intended semantic version.
3. Dependency review, license policy, and `Cargo.lock` are current.
4. The relevant native platform gates are complete. Cross-compilation is not a substitute for native Intel macOS validation.
5. A release reviewer is prepared to run [the local RC checklist](release-candidate-qa.md) against the exact draft assets.

Do not create a public tag or release for a target whose required native gate is still blocked.

## Candidate workflow

Run **Release artifacts** with an empty `tag` input on the exact candidate ref. The workflow:

- builds all six reviewed target triples from one commit;
- downloads only the pinned checksummed uv asset for each matching target;
- invokes `scripts/package-release.sh` separately for each target;
- verifies all archive sidecars, manifests, source commits, targets, three packaged binary hashes, and uv license files;
- performs a first-use and restart managed-IPython smoke test with the packaged uv on each native runner (Intel macOS remains a separate native gate);
- uploads one short-lived combined candidate artifact; and
- performs no GitHub release mutation.

Download the combined artifact, verify `SHA256SUMS`, and inspect any failed matrix job before retrying. A retry rebuilds archives and may produce different gzip bytes, so evidence must name the exact downloaded artifact and digest.

## Signed tag and draft

After candidate review, create an annotated signed tag whose version exactly matches `Cargo.toml`:

```sh
git switch main
git pull --ff-only
git status --short
git tag -s v0.1.0 -m "Morons v0.1.0"
git verify-tag v0.1.0
git push origin v0.1.0
```

Run **Release artifacts** from `main` with that existing tag in the `tag` input. The workflow fails unless:

- the input has exact `vMAJOR.MINOR.PATCH` form;
- its version matches the workspace version;
- it is an annotated tag whose signature GitHub marks verified;
- its commit is contained in `origin/main`; and
- no GitHub release already exists for the tag.

On success, the workflow creates an **unpublished draft** containing six `.tar.gz` archives and `SHA256SUMS`. If draft creation or asset upload has an uncertain outcome, inspect the Releases page and workflow artifacts; do not blindly rerun or overwrite an existing release.

## Exact-asset QA

Download the draft assets while authenticated:

```sh
mkdir -p /tmp/morons-release-review
gh release download v0.1.0 --dir /tmp/morons-release-review
(
  cd /tmp/morons-release-review
  sha256sum -c SHA256SUMS
)
```

Use `shasum -a 256 -c SHA256SUMS` on systems without `sha256sum`. Run `docs/release-candidate-qa.md` against these exact archives and attach the completed result record to the release review. Do not substitute local `target/release` binaries or earlier candidate-workflow archives.

The draft remains unpublished when a required target fails, a checksum differs, an archive has the wrong contents, a native gate is missing, or the checklist has an unexplained failure.

## Publish

After reviewing the complete QA record and draft contents, update the draft notes with installation requirements, data-use disclosures, known blocked optional paths, and checksums. Then publish explicitly:

```sh
gh release edit v0.1.0 --draft=false --latest
```

Confirm from an unauthenticated view that the release, tag, six archives, and checksum file are visible. Download one asset again and recheck its digest.

## Post-release

- Confirm no release workflow, package process, companion, kernel, or QA fixture remains running.
- Keep the signed tag immutable. Never move or replace a published release tag.
- Fix packaging or application defects in a new reviewed commit and version; do not silently replace published assets.
- Record native target coverage and any target withheld from support language.
