# Releasing Morons

This is the maintainer procedure for producing the six target archives defined by [ADR 0007](adr/0007-supported-processor-architectures.md). The workflow creates candidate artifacts or an unpublished GitHub draft. It never publishes a release automatically.

## Candidate preconditions

1. The exact candidate commit is on `main`, the checkout is clean, and all required CI/security checks pass.
2. `Cargo.toml` contains the intended semantic version.
3. Dependency review, license policy, and `Cargo.lock` are current.
4. Known native-platform gaps and optional-path blockers are recorded rather than represented as passes.
5. A release reviewer is prepared to run [the local RC checklist](release-candidate-qa.md) against the exact workflow artifact.

A candidate-only workflow may run while an explicitly recorded native gate remains blocked. Do not create a public tag or GitHub release until every target being claimed as release-supported has passed its required native gate.

## Candidate workflow

Run **Release artifacts** with an empty `tag` input on the exact candidate ref. The workflow:

- builds all six reviewed target triples from one commit;
- downloads only the pinned checksummed uv asset for each matching target;
- invokes `scripts/package-release.sh` separately for each target;
- verifies all archive sidecars, manifests, source commits, targets, three packaged binary hashes, and uv license files;
- performs a first-use and restart managed-IPython smoke test with the packaged uv on each native runner (Intel macOS remains a separate native gate);
- uploads one short-lived combined candidate artifact; and
- performs no GitHub release mutation.

After a successful run, download the combined artifact by exact run and artifact name, then verify its contents:

```sh
RUN_ID=REPLACE_WITH_RUN_ID
ARTIFACT=morons-release-REPLACE_WITH_VERSION-REPLACE_WITH_12_CHARACTER_COMMIT
REVIEW_DIR=$(mktemp -d)
gh run download "$RUN_ID" \
  --name "$ARTIFACT" \
  --dir "$REVIEW_DIR"
(
  cd "$REVIEW_DIR"
  sha256sum -c SHA256SUMS
)
```

Use `shasum -a 256 -c SHA256SUMS` on systems without `sha256sum`. Record the run URL, exact source commit, artifact name, GitHub-reported aggregate artifact digest, and all hashes from `SHA256SUMS`. Candidate artifacts expire, so retain the review record rather than treating the Actions artifact as durable release storage.

Inspect any failed matrix job before retrying. A retry rebuilds archives and may produce different gzip bytes, so earlier artifact evidence does not apply to the retry. Any later source change also invalidates the candidate and requires a new workflow run.

## Release preconditions, signed tag, and draft

Before creating or pushing a tag, confirm every required native target gate is complete, the candidate has no unexplained failure, the version is final, and no further source change is planned. Cross-compilation is not a substitute for native Intel macOS validation.

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

## Exact signed-tag asset QA

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
