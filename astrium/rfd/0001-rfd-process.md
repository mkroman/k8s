---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: published
discussion: "https://github.com/mkroman/k8s/pull/435"
labels: [process, software]
---

# RFD 0001: RFD process

## Summary

Astrarium uses Requests for Discussion (RFDs) to record decisions that are
expensive to reverse: public interfaces, the manifest schema, the rendering
contract, the CI and release model, and the rules that keep the codebase
coherent as several agents work in parallel. This document defines the states,
the branch-and-pull-request lifecycle, the Markdown front matter, and the index.

The process is adapted from [Oxide Computer's RFD 0001][oxide-rfd-1]. The
states and lifecycle are intentionally identical. The adaptation is mechanical:
Astrarium is written in Markdown and uses YAML front matter instead of AsciiDoc
and attribute lines.

[oxide-rfd-1]: https://rfd.shared.oxide.computer/rfd/0001

## Motivation

The repository already has a working e2e harness and a production app-of-apps
catalogue. What it does not have is a durable explanation of why the harness is
shaped the way it is, which trade-offs were considered, or which parts are
load-bearing. That knowledge currently lives in shell comments, pull request
threads, and the heads of the people who wrote them.

Astrarium makes that problem worse before it makes it better. It introduces a
new binary, a new manifest format, a new renderer, a chart repository, a kind
bootstrap, a registry mirror, and a telemetry model. Each of those has credible
alternatives. Without a written record, the next contributor will reasonably
ask "why not just extend `run-app.sh`?" and there will be no answer in the
repository.

RFDs also solve a coordination problem specific to this project. Several agents
work in separate worktrees against shared interfaces. An RFD is the stable
artifact they can read before touching code. It is cheaper to disagree with a
paragraph than with a merged module.

An RFD is not a specification. A specification says what a conforming
implementation must do; an RFD says what we decided, what we gave up, and what
we still do not know. RFDs are allowed to be wrong in ways that specifications
are not, provided the wrongness is visible and dated.

## Guide-level explanation

### What belongs in an RFD

Write an RFD when a change fixes an interface that other crates, repositories,
or people will depend on, when it chooses between alternatives with long-lived
consequences, or when the reasoning will be non-obvious later. The current set
is a good calibration:

- RFD 0002 fixes the product shape: name, goals, non-goals, crates, commands.
- RFD 0003 fixes `astrarium.toml` and the selection algorithm.
- RFD 0004 fixes how applications reach Argo CD.
- RFD 0005 fixes the cluster, registry, and telemetry boundary.
- RFD 0006 fixes CI, releases, and how e2e jobs obtain the binary.
- RFD 0007 fixes code structure and the testing contract.

Do not write an RFD for a bug fix, a dependency bump, a new app under `apps/`,
or a refactor that preserves every interface. Those are pull requests. If a
change starts to feel like it needs an RFD halfway through, stop and write one;
the review will be cheaper than the rework.

### States

| State | What it means | Exit |
| --- | --- | --- |
| `prediscussion` | The author is still drafting. Force-pushes are expected. Nobody is being asked to review. | Author moves to `ideation` or `discussion`. |
| `ideation` | Early input is welcome, but the direction is fluid. Not binding on implementation. | Author moves to `discussion` when the questions are sharp enough to answer. |
| `discussion` | Formal review. The author has named the open questions and asked specific reviewers. | Maintainer approves; author sets `published` immediately before merge. |
| `published` | The direction is agreed. New work should follow it, even if no code exists yet. | Implementation lands; author moves to `committed`. Or the decision is reopened. |
| `committed` | The code implements the RFD and the text has been checked against it. | Stays until superseded. |
| `abandoned` | Deliberately not pursued. Kept for the reasoning, not deleted. | A new RFD is required to revive the idea. |

`published` is the state that most often causes confusion. It does not mean
"implemented", and it does not mean "perfect". It means "we have stopped
arguing about the direction and will implement this unless new evidence
appears". An RFD can be `published` for months before the first line of code.

`committed` is not automatic on merge. The author opens a follow-up PR after
the implementation lands, rereads the RFD, fixes the places where reality
diverged, and changes the state. If the divergence is material, the RFD goes
back to `discussion` instead.

### Lifecycle

RFDs are never pushed directly to `main`. The branch is the unit of review, and
the pull request is the discussion venue.

1. `git switch -c rfd/NNNN-short-slug`
2. Copy `template.md`, set `state: prediscussion`, fill in the front matter.
3. Add the index row in `README.md` in the same commit.
4. Open a draft PR. The draft is the `prediscussion` phase.
5. When ready, set `state: discussion`, set `discussion` to the PR URL, and
   request reviewers.
6. Apply review conclusions to the document. Resolve every question marked
   "must answer before published"; defer the rest explicitly.
7. Set `state: published`. A maintainer approves and merges.
8. After implementation, a follow-up PR sets `state: committed`.

The bootstrap set (0001–0007) lands as one PR because the index and the process
need an initial corpus. That is a one-time exception, recorded here so it is
not cited as precedent.

### Review

Reviewers are chosen for the decision, not for seniority. For RFD 0003 that is
whoever owns the manifest and the e2e suite; for RFD 0006 it is whoever owns
the workflows. A review is complete when:

- every open question is either answered or explicitly deferred with an owner;
- the alternatives section contains the alternative a reasonable reviewer
  would have proposed, and says why it lost;
- the reference-level section is specific enough to implement without asking
  the author which reading was intended;
- the drawbacks are real. "Adds some complexity" is not a drawback; "the
  wrapper chart hides Kustomize's own health assessment from Argo CD" is.

Approval is lazy consensus with a maintainer's explicit approval on the PR. A
reviewer who disagrees must say which alternative they prefer and why. "I don't
like it" is not a review outcome; it is the start of one.

### Editing published RFDs

Published RFDs are living documents, but not silently. Typos, dead links, and
clarifying examples go through a focused PR with no state change. A change to
the decision itself either returns the RFD to `discussion` (if implementation
has not started) or is written as a new RFD that supersedes the old one. The
old RFD stays in the index with its original state and a link forward.

This matters more than it sounds. The index is only useful if a reader can
trust that a `published` RFD describes the current agreement. Allowing quiet
edits to `published` documents destroys that trust faster than allowing them to
be wrong.

### The index

`README.md` is the index. It is ordered by number and contains the title, the
state, and a one-sentence summary. The state in the table and the state in the
front matter are updated in the same commit. The title in the table matches the
`#` heading. The summary describes the decision, not the document ("Defines
`astrarium.toml` and diff-based selection", not "This RFD talks about the
manifest").

An RFD that is `abandoned` stays in the table. Deleting it loses the reason the
idea was rejected, which is the main reason to keep an RFD at all.

## Reference-level explanation

### Front matter

Every RFD begins with a YAML front matter block delimited by `---`:

```yaml
---
authors: ["Astrarium maintainers <maintainers@astrarium.invalid>"]
state: prediscussion
discussion: ""
labels: [process, software]
---
```

The fields are:

| Field | Type | Constraint |
| --- | --- | --- |
| `authors` | array of strings | Non-empty. Each entry is `Name <email>`. |
| `state` | string | Exactly one of `prediscussion`, `ideation`, `discussion`, `published`, `committed`, `abandoned`. |
| `discussion` | string | A URL, or the empty string only when `state` is `prediscussion`. |
| `labels` | array of strings | Non-empty. Lowercase, hyphenated if needed. |

Unknown fields are rejected. This is the same rule as `astrarium.toml` (RFD
0003): a typo in a state name must fail loudly, not silently produce an RFD
that no tool or reader recognizes.

The `.invalid` domain in the bootstrap set marks an unassigned human owner.
Before an RFD becomes `published`, `authors` names the person or people who are
accountable for it.

### File naming and numbering

Files are `astrium/rfd/NNNN-short-slug.md`, where `NNNN` is a zero-padded
four-digit number and `short-slug` is lowercase ASCII with hyphens. The number
is allocated when the pull request is opened and never changes after review
starts. If two RFDs claim the same number, the one that merges first keeps it;
the other rebases and renumbers before it is reviewed.

Numbers are not reused. An abandoned RFD keeps its number forever. This makes
references stable: "see RFD 0004" means the same document in every commit,
issue, and code comment.

`template.md` is not an RFD. It has no number and is not in the index.

### State transitions

The allowed transitions are:

```text
prediscussion -> ideation -> discussion -> published -> committed
      |             |            |            |
      +-------------+------------+------------+--> abandoned
```

A transition is a pull request that changes the front matter and the index row.
The only transition that may also change the body substantially is
`discussion -> published`, where review conclusions are applied. Every other
transition should be a small diff; if it is not, the RFD is being rewritten and
belongs in `discussion`.

`abandoned` is reachable from any state. It requires a sentence in the
`Rationale and alternatives` or `Summary` section saying why the idea is not
being pursued. An abandoned RFD is not deleted and its number is not reused.

### Normative language

`MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, and `MAY` are used with their
ordinary RFC 2119 meanings. Most RFD prose is descriptive and uses none of
them. A `MUST` in an RFD is a constraint on the implementation and should be
testable; a `MUST` that cannot be tested is a preference in disguise.

### Cross-references

RFDs refer to each other by number: "see RFD 0003", not "see the manifest
document". The number is stable; the title is not. A reference to a section
may add the heading: "RFD 0004, Rendering and Argo CD". Links to files are
allowed but should point at a path that is expected to survive; link to the
RFD when the decision is what matters.

### What an RFD is not

- It is not a changelog. Git history records what changed; the RFD records why.
- It is not a design doc for a single PR. If the decision does not outlive the
  PR, put it in the PR description.
- It is not a specification. The code and its tests are the specification; the
  RFD explains the contract the tests are protecting.
- It is not a vote. Comments inform the maintainer's decision; they do not
  outnumber it.

## Drawbacks

- Writing an RFD costs time before any code exists. For a small project this is
  real overhead, and the threshold in this document is deliberately high
  enough that most changes do not need one.
- A `published` RFD can become stale if the implementation diverges and nobody
  opens the `committed` PR. The follow-up step is a social obligation, not an
  enforced one.
- The index duplicates the title and state that already exist in the front
  matter. The duplication is intentional, because GitHub renders the README
  without parsing front matter, but it is a place for the two to disagree if a
  reviewer is careless.
- A single-maintainer repository can turn "lazy consensus" into "the
  maintainer's opinion". The alternatives section and the requirement to name a
  rejected alternative are the only safeguards, and they are weak.
- Markdown front matter is not as expressive as AsciiDoc attributes for things
  like revision history or multiple discussion links. If the process grows,
  the front matter will need a schema and a validator.

## Rationale and alternatives

**AsciiDoc with attribute lines (Oxide's format).** Rejected because the
repository is Markdown-first, GitHub renders Markdown natively, and adding an
AsciiDoc toolchain for seven documents is not justified. The semantics of the
states are unchanged; only the syntax of the metadata differs.

**Architecture Decision Records (ADRs).** ADRs are shorter and decide one
thing at a time. They are a good fit for a codebase where decisions are local.
Astrarium's decisions are cross-cutting (manifest, renderer, CI, telemetry) and
benefit from the longer "guide-level then reference-level" structure. ADRs also
usually have no discussion state, which is the part that matters here.

**Issues or a wiki.** Rejected because they are not reviewed with the code, do
not have a stable number in the repository, and cannot be validated in CI. An
issue thread is a discussion, not a decision record.

**No process.** This is the status quo. It works while one person holds the
whole design in their head. It fails when agents and contributors work in
parallel, and it fails again when the person leaves.

**A single `DESIGN.md`.** Rejected because it becomes a monolith that nobody
reads end to end, and because every change touches every section. Separate RFDs
let a reader load only the decision they need.

## Prior art

- [Oxide RFD 0001][oxide-rfd-1] is the direct source. The states, the
  branch-and-PR lifecycle, the index, and the rule that published RFDs remain
  editable all come from it.
- Rust RFCs use a similar pull-request-and-final-comment-period model. The
  difference is that Rust RFCs are closer to specifications and are expected
  to be normative for the language; Astrarium RFDs are design arguments.
- Kubernetes Enhancement Proposals (KEPs) separate the design from the
  implementation plan and have explicit graduation criteria. The milestone
  structure in RFD 0002 borrows that idea without the KEP process weight.
- Nygard's ADR pattern contributes the "record the rejected alternative"
  discipline. This process keeps the discipline but not the one-page format.

## Unresolved questions

- **Must answer before `published`:** should a CI check validate the front
  matter, the state vocabulary, and the index against the files? The argument
  for is consistency; the argument against is that adding a parser and a
  workflow for seven documents is more machinery than the problem deserves. A
  shell check that greps the state line and the table row may be enough.
- **Can be deferred:** should `discussion` allow a list of URLs (PR, issue,
  meeting notes) instead of a single string? A single URL is simpler and
  forces one canonical venue; a list would accommodate design discussions that
  happen in more than one place.
- **Can be deferred:** should an RFD be able to declare `supersedes: 0004` and
  have the index render the relationship? Today the relationship is a sentence
  in the body. A structured field would make it queryable, but nothing queries
  it yet.
- **Can be deferred:** is `ideation` earning its place? It is rarely used in
  Oxide's corpus and may be a state that authors skip. Removing it would leave
  five states and a simpler diagram.

## Future possibilities

- Generate the index from front matter instead of maintaining the table by
  hand, once the number of RFDs makes manual updates error-prone.
- Publish the RFDs as a static site, preserving the Markdown source of truth.
- Add a `committed` check that links each RFD to the implementation PR and the
  test that proves the behaviour, so the state is backed by evidence rather
  than a claim.
- Reuse the same front matter schema for chart READMEs or app documentation if
  a second document type needs the same lifecycle.
