# Astrarium RFDs

An RFD is a **Request for Discussion**. It is a written argument for a change,
not a specification and not a promise. RFDs exist so that the reasoning behind
Astrarium's architecture survives longer than a pull request comment, and so
that disagreements are resolved in prose before they are resolved in code.

This process is adapted from [Oxide Computer's RFD 0001][oxide-rfd-1]. The
states, the branch-and-pull-request lifecycle, and the index are deliberately
the same. The only material adaptation is that Astrarium writes RFDs in
Markdown with YAML front matter instead of AsciiDoc with attribute lines.

[oxide-rfd-1]: https://rfd.shared.oxide.computer/rfd/0001

## States

Every RFD declares exactly one state in its front matter. The index table and
the front matter must agree; a pull request that changes one changes the other.

| State | Meaning | Who moves it |
| --- | --- | --- |
| `prediscussion` | A draft. The author is still writing, may force-push, and is not asking for review yet. The `discussion` link is empty. | Author |
| `ideation` | Shared for early input. The direction is still fluid and nothing in the document is binding. | Author |
| `discussion` | Under formal review. The author has asked specific people to resolve the open questions. | Author, when the PR is ready for review |
| `published` | Agreed direction. The RFD is normative for new work but may not be implemented yet. This is the state an RFD has when its PR merges. | Maintainer approval at merge |
| `committed` | Implemented and verified. The document describes what the code actually does. | Author, in a follow-up PR after implementation |
| `abandoned` | Deliberately not pursued. The document stays in the repository so the reasoning is not lost. | Author proposes, maintainer approves |

An RFD may move backwards. A `published` RFD that turns out to be wrong goes
back to `discussion` through a focused change PR; it is not quietly rewritten.
`committed` is not a synonym for "merged": it means the behaviour exists and
the RFD has been checked against it.

## Lifecycle

RFDs are never pushed directly to `main`. Every RFD, including a one-line
state change, lands through a pull request.

1. Branch from `main` as `rfd/NNNN-short-slug`.
2. Copy `template.md` to `NNNN-short-slug.md`, fill in the front matter with
   `state: prediscussion`, and add a row to the index below.
3. Open a draft pull request. When the document is ready for review, change the
   state to `discussion`, set `discussion` to the pull request URL, and request
   reviewers.
4. Resolve the open questions in the pull request. Review comments that change
   the decision are applied to the document, not left only in the thread.
5. Before merge, change the state to `published`. A maintainer approves and
   merges. The index row is updated in the same commit.
6. When the implementation lands, open a small follow-up PR that changes the
   state to `committed` and corrects any text that no longer matches reality.

The bootstrap set (RFDs 0001–0007) is an exception to the one-RFD-per-PR rule:
the process document and the initial architecture need to land together so the
index is not empty. Each still has its own file, its own state, and its own
index row. After this set, one RFD per PR is the norm.

An RFD is not a substitute for a design doc in a code review. Small changes do
not need an RFD. A change needs one when it fixes an interface that other
crates, other repositories, or other people will depend on, when it chooses
between credible alternatives with long-lived consequences, or when the reason
for the choice will be non-obvious in six months. RFD 0002 through RFD 0007 are
the current examples of that threshold.

## Adding an RFD

```bash
git switch -c rfd/0009-probe-timeouts
cp astrium/rfd/template.md astrium/rfd/0009-probe-timeouts.md
$EDITOR astrium/rfd/0009-probe-timeouts.md   # set state: prediscussion
$EDITOR astrium/rfd/README.md                # add the index row
git add astrium/rfd
git commit -m "rfd: add 0009 probe timeouts"
gh pr create --fill
```

Rules for the number:

- Numbers are four digits, zero-padded, and never reused or renumbered.
- Take the next free number at the time you open the pull request. A competing
  RFD that merges first keeps its number; rebase and renumber yours before
  review starts, not after.
- The file name is `NNNN-short-slug.md`. The slug is lowercase, hyphenated, and
  descriptive; it is not a sentence.
- The `#` heading in the document is the title that appears in the index.

## Front matter

```yaml
---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [software, testing]
---
```

| Field | Required | Meaning |
| --- | --- | --- |
| `authors` | yes | One or more `Name <email>` entries. The people accountable for the document, not everyone who commented. |
| `state` | yes | One of the six states above. |
| `discussion` | yes | The pull request or issue URL where the RFD is being discussed. Empty only while `prediscussion`. |
| `labels` | yes | At least one label. Prefer the existing vocabulary: `software`, `process`, `testing`, `ci`, `observability`, `security`. |

The `.invalid` domain in the bootstrap set is a deliberate placeholder: these
drafts were produced by an agent under maintainer direction, and attributing
them to a person would be wrong. Replace `authors` with the real owner before
the state becomes `published`.

## Editing a published RFD

`published` does not mean frozen. Typos, broken links, clarified examples, and
corrections that do not change the decision are welcome through focused change
PRs. The state stays `published` and the index does not change.

A change that alters the decision is different:

- If the implementation has not started, reopen the RFD: set the state back to
  `discussion`, explain the change in the PR, and get approval again.
- If the implementation exists and the change is substantial, write a new RFD
  that supersedes the old one. Mark the old one `abandoned` only if it is no
  longer true at all; otherwise leave it `committed` and link forward.
- Never rewrite history silently. The Git history of the RFD is part of the
  record.

## Index

The table below is the canonical index. It is ordered by number. A pull request
that adds an RFD, changes its state, or changes its title updates this table in
the same commit. The summary is one sentence, not a paragraph, and describes
the decision rather than the document.

| RFD | Title | State | Summary |
| --- | --- | --- | --- |
| [0001](0001-rfd-process.md) | RFD process | published | Adapts Oxide's RFD process to Markdown front matter and fixes the states, review rules, and index maintenance. |
| [0002](0002-astrarium-overview.md) | Astrarium overview | published | Names the project, states its goals and non-goals, and fixes the crate, command, and milestone shape. |
| [0003](0003-manifest-and-selection.md) | Manifest and selection | published | Defines `astrarium.toml`, the dependency DAG, and diff-based application selection. |
| [0004](0004-rendering-and-argocd.md) | Rendering and Argo CD | published | Packages Helm and Kustomize applications into an HTTP chart repository and creates Argo CD resources directly. |
| [0005](0005-cluster-registry-telemetry.md) | Cluster, registry, and telemetry | published | Bootstraps kind, the path-prefixed registry proxy, Argo CD, and OTLP traces with bounded diagnostics. |
| [0006](0006-ci-release-and-caching.md) | CI, release, and caching | published | Defines GitHub Actions, release tags and binaries, and how e2e jobs obtain Astrarium. |
| [0007](0007-testing-and-structure.md) | Testing and structure | published | Fixes the workspace, module, error, async, and testing rules and the migration away from `hack/e2e`. |
| [0008](0008-chaos-testing.md) | Chaos testing for stateful upgrades | ideation | Plans a repeatable outage, failure-recovery, and Secret-rotation suite for stateful upgrade paths. |

## Conventions

- Write in the present tense for what exists and the future tense for what is
  proposed. "Astrarium renders" describes code; "Astrarium will render"
  describes this document's intent.
- Use `MUST`, `SHOULD`, and `MAY` only where the distinction matters. Most of
  an RFD is explanation, not requirement.
- Cross-reference other RFDs by number, for example "see RFD 0003". Do not
  duplicate a schema or a table that another RFD owns; link to it.
- Prefer concrete examples: a TOML fragment, a span attribute list, a workflow
  excerpt. An RFD without examples is usually hiding an unresolved design.
- Record the alternatives that were rejected and why. A future reader who
  proposes the same alternative should find the answer here.
- Keep secrets, tokens, kubeconfigs, and real customer data out of RFDs.
