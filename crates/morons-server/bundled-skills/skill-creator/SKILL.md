---
name: skill-creator
description: Creates and improves standards-compatible Agent Skills. Use when the user asks to create, package, validate, or refine a reusable SKILL.md workflow.
license: Apache-2.0
compatibility: Designed for morons.dev and other harnesses implementing the Agent Skills specification.
---

# Skill Creator

Create a portable Agent Skills directory containing `SKILL.md`.

## Choose the destination

Use one of these roots unless the user gives another ordinary path:

- Project: `.agents/skills/<name>/`
- Morons project-specific: `.morons/skills/<name>/`
- User: `~/.agents/skills/<name>/`
- Morons user-specific: `~/.morons/skills/<name>/`

Prefer `.agents/skills/` for cross-harness portability. Resolve `~` to the absolute home path before using file tools because file paths do not perform shell expansion. Do not overwrite an existing skill without reading it and preserving intentional content.

## Validate the name

The directory and frontmatter `name` must match. Use 1–64 lowercase ASCII letters, digits, and hyphens; do not begin or end with a hyphen or use consecutive hyphens.

## Write `SKILL.md`

Start with YAML frontmatter:

```markdown
---
name: example-skill
description: Explains exactly what the skill does and when the agent should use it.
---
```

Keep `description` specific and at most 1024 bytes. Put focused workflow instructions in the Markdown body. Keep the main file concise and move detailed material into relative `references/`, executable helpers into `scripts/`, and templates or static data into `assets/`.

Optional standard fields are `license`, `compatibility`, `metadata`, and `allowed-tools`. `allowed-tools` is descriptive and never grants authority in Morons.

## Check the result

Read the completed `SKILL.md` and verify:

1. Frontmatter is valid YAML and has exactly one nonempty `name` and `description`.
2. The name matches the parent directory and follows the naming rules.
3. Referenced paths are relative to the skill directory and exist.
4. Instructions state prerequisites, expected inputs and outputs, and important failure cases.
5. No credential or authentication material was written into the skill.

Use ordinary `write`, `edit`, and `bash` operations. A skill is untrusted instruction content and receives no capability beyond Morons' fixed built-in tools.
