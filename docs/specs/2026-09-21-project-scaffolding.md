# Groundhog project scaffolding

- Status: Draft for review
- Date: 2026-09-21
- Scope: `groundhog new`, template discovery, a complete counter application,
and prompt-driven project creation.

## 1. Goal

Let developers start with a minimal contract, a working example, or a description.
Every route must produce a project that follows the same Groundhog build, test,
code generation, and deployment conventions.

The initial template catalog contains exactly two entries: `counter` and
`counter-app`. Creating a project without `--template` produces a minimal
contract scaffold. The minimal scaffold is the base project, not a named template.

Prompt-driven creation is a separate track. Its first phase selects and proves
an agent execution model. Template delivery does not depend on that decision.

## 2. Current behavior

As of CLI 0.18.0:

- `groundhog new <NAME>` defaults to the `counter` template.
- `--template` accepts `counter` and `stub`.
- Both create a single Rust contract with native, RV32, and integration-test files.
- The command creates a new directory and rejects an existing destination.
- Template selection is hard-coded in `src/main.rs` and `src/scaffold.rs` under
  `crates/tools/cli/`.
- Templates contain no TypeScript application or connected browser workflow.
- There is no template catalog command or prompt-driven creation mode.

Existing foundations include `groundhog build --bindings ts`, the TypeScript
client, local-node tooling, structured CLI output, and `groundhog --skill`.
Reuse these public interfaces rather than recreating their behavior in templates.

## 3. Product decisions

1. No template is selected by default. The base scaffold is minimal and buildable.
2. `--template` is an optional override with two initial values: `counter` and
   `counter-app`.
3. Templates are discoverable from the CLI, including through JSON output.
4. `counter-app` includes working contract, TypeScript, and browser application code.
5. Official templates ship with the CLI and use a tested set of dependency versions.
6. Future templates use the same catalog and generation system.
7. `--prompt` uses the same project conventions and verification commands.
8. Agent execution remains an explicit research decision, not an assumed provider.

### Scope limits

This workstream does not add token, escrow, or AMM templates, remote template
repositories, a template marketplace, or a frontend framework selector. An AMM
is an example prompt, not a promised built-in implementation.

The first application targets local development. Remote deployment and production
wallet onboarding can build on this workflow later.

## 4. Proposed CLI contract

The commands in this section describe the target interface, not current behavior.

```console
groundhog new my-contract
groundhog new my-counter --template counter
groundhog new my-app --template counter-app
groundhog templates list
groundhog templates list --json
groundhog templates show counter-app
groundhog new my-amm --prompt "A constant-product AMM with deposits and swaps"
```

### Base project

`groundhog new <NAME>` creates a minimal contract with an empty initializer,
working build configuration, and tests for that minimal behavior. It contains
no counter logic or unused example exceptions. It retains the SDK feature names,
RV32 wrapper, Cargo overlays, and self dev-dependency required by the toolchain.

Keep existing project-name validation and destination checks. Template generation
does not install dependencies, build contracts, start nodes, or deploy contracts.
It prints the exact next commands for the selected project.

### Template selection and discovery

`--template <ID>` selects a complete project recipe. Reject unknown IDs before
creating files and direct the user to `groundhog templates list`.

`templates list` shows each template's ID, summary, and project kind. It also
explains that omitting `--template` creates a minimal contract.

`templates show <ID>` shows its contents, prerequisites, and startup workflow.
Both commands support `--json` and the existing global output conventions.
Listing and inspecting embedded templates requires no network access.

The catalog is the source of truth for accepted IDs, help text, listing,
inspection, and generation. Do not maintain separate template-name lists.

### Compatibility and structured output

Changing the default is intentional. Update getting-started commands and release
notes so examples that need a counter explicitly use `--template counter`.

Remove `stub` from the public template catalog. If passed explicitly, reject it
before creating files and explain that `groundhog new <NAME>` now creates the
minimal project. Do not silently interpret it as a different named template.

Define typed, versioned reports for template discovery and project creation.
For ordinary creation, preserve the existing project name, path, and config path
information; represent no selected template with `template: null`. Bump the
existing `new` data schema from v1 to v2 for this change. Document the schema
before implementation and follow the CLI's `CommandResult` envelope conventions.

Keep `--offline`'s existing meaning: it prevents managed toolchain downloads.
It does not promise that later Cargo or package-manager commands avoid the network.

## 5. Template architecture

Use a small embedded catalog with a shared base scaffold and template-specific
files. Each entry describes:

- Stable ID, summary, and project kind (`contract` or `application`).
- Files and substitutions required to generate the project.
- Required tools and supported dependency versions.
- Next-step commands and verification commands.

Keep metadata and generation together so listing cannot advertise a template
that the generator cannot create. Share the counter contract source and tests
between `counter` and `counter-app` to prevent behavioral drift.

Support whole-project file trees rather than assuming every template is a fixed
tuple of Rust files. Keep the implementation small; a generic plugin engine is
unnecessary for two templates.

Validate the name, template, substitutions, and destination before writing.
If generation fails, report the destination and partial state clearly. Never
overwrite an existing project. Embedded assets must be included in CLI packages.

Adding a future template should require a catalog entry, its files, and its
verification coverage. It should not require a new CLI flag or dispatch branch.

## 6. Counter application

### User outcome

After creating `counter-app`, the developer can follow the printed commands to
start a local node, build the contract and bindings, deploy and initialize the
counter, and open the application. No manual address copying, ABI editing, or
wallet-call construction is required.

The application must:

- Show the active local endpoint and deployed contract address.
- Read the counter through the generated TypeScript binding.
- Increment and decrement it through the supported wallet invocation path.
- Show submission, pending, successful execution, and failure states accurately.
- Refresh the value after successful execution.
- Handle an unavailable node, missing deployment, and a rejected operation.

Transaction submission alone must not appear as successful contract execution.
Decode the wallet and application outcomes using the supported client APIs.

### Proposed project structure

Keep the contract at the project root initially, matching existing CLI project
discovery. Confirm this layout during the first app implementation spike.

```text
my-app/
  Cargo.toml
  build.rs
  groundhog.toml
  rust-toolchain.toml
  AGENTS.md
  README.md
  .cargo/
  src/                    # Rust contract and RV32 wrapper
  tests/                  # Native, RV32, and RPC tests
  package.json            # Root application workflow commands
  scripts/                # Local setup, deployment, and process lifecycle
  app/
    package.json
    src/
      generated/          # Bindings emitted from the contract specification
      ...                 # Browser application and client setup
  .groundhog/             # Ignored local deployment/runtime state
```

Generated bindings are build outputs, not a hand-maintained interface. Use the
public TypeScript client and protocol packages. Resolve compatible published
versions and current registry access requirements before freezing the template.

The initial recommendation is React, Vite, TypeScript, and npm. Confirm this
choice and supported Node version in the app phase; ship one tested stack and
a lockfile. Do not introduce framework options in this workstream.

### Local workflow contract

The target user workflow is:

```console
groundhog new my-app --template counter-app
cd my-app
npm install
npm run dev
```

These package scripts are proposed additions. `npm run dev` must coordinate:

1. Start a dedicated local node through supported Groundhog tooling.
2. Wait for readiness using structured output and runtime information.
3. Build the RV32 artifact and generate TypeScript bindings.
4. Set up the supported local-development wallet and deploy the counter.
5. Initialize the counter and confirm successful execution.
6. Write local app configuration from actual endpoint and deployment results.
7. Start the frontend and print its URL.

On shutdown, stop processes the workflow owns. On startup failure, report the
failed stage and clean up owned processes. Do not stop a node owned by the user.
The initial workflow may use an ephemeral chain, but it must document reset
behavior and replace stale deployment configuration after each fresh start.

Use structured CLI/client results instead of parsing terminal status messages.
Browser configuration contains public local connection data only; do not copy
user signing keys into frontend source. Select and test the local wallet adapter
and browser RPC access, including CORS, during the app spike.

Keep lifecycle code thin and reusable. Reuse existing node supervision and
deployment APIs. If a required public interface is missing, identify and add
the smallest shared capability rather than embedding a second implementation.

### Verification

The generated project provides commands for native/RV32 contract tests,
TypeScript type checking, frontend build, and an automated end-to-end test.
The end-to-end test must start from a clean local chain and:

1. Build the contract and regenerate bindings.
2. Deploy and initialize it through the generated workflow.
3. Read the initial value through TypeScript.
4. Submit an increment through the wallet and verify execution and updated state.
5. Exercise a failure, such as decrementing below zero, without reporting success.
6. Exercise the browser controls and confirm the displayed value updates.
7. Shut down all test-owned processes.

Also verify that a second fresh startup does not reuse stale addresses. Tests
must exercise the public client and real local node, not only mocked RPC calls.

## 7. Prompt-driven creation

### Proposed interface and behavior

```console
groundhog new my-amm --prompt "A constant-product AMM with deposits and swaps"
```

Keep `<NAME>` required initially so the destination is explicit. Require a
nonempty prompt. Initially make `--prompt` and `--template` mutually exclusive;
the agent can select an appropriate base using catalog metadata. Combining them
can be considered after the first agent workflow is proven.

Groundhog owns project validation, scaffold conventions, version-matched
guidance, and the final verification report. The selected agent runtime owns
model interaction and implementation work.

The target flow is:

1. Validate the destination and agent prerequisites before creating files.
2. Establish the requested scope and resolve material ambiguity through questions
   or recorded assumptions. An AMM prompt does not define fees or pool behavior.
3. Create a known-good base project.
4. Supply the prompt, `groundhog --skill`, project instructions, template catalog,
   dependency versions, and verification commands to the agent.
5. Let the agent implement the contract, tests, and application when requested.
6. Run the relevant checks and report their actual results.

Completion must distinguish verified success, incomplete generation, and failed
verification. A provider response or agent process exit alone is not proof of a
working project. On interruption or failure, preserve generated work and report
its path, completed steps, and how to continue.

Define how agent output, questions, cancellation, and final results coexist
with `--json` before release. Machine-readable output must remain parseable.
Prompt execution must not silently submit work to a paid provider or choose a
different installed agent when the configured runtime is unavailable.

### Prompt phase 1: select and prove the execution model

Compare these approaches with a small prototype:

| Approach | Questions to resolve |
| --- | --- |
| Direct AI API integration | Where does the user configure their API key? Who owns the tool loop, model choice, limits, and session state? |
| Installed coding agent | Can Claude Code, Codex, or OpenCode accept project context, support questions, expose results, and handle interruption reliably? |
| Both behind one interface | Is the extra scope justified now, and what is the smallest common execution contract? |

The phase must produce a written decision, a working prototype of the selected
approach, and a concrete CLI/configuration proposal. Supporting all three coding
agents or multiple API providers is not a prerequisite for the first release.

Evaluate setup cost, existing user authentication, interactive and unattended
use, context delivery, observable verification, cancellation, and recovery.
Keep credentials out of command-line arguments, generated files, and reports.

Open questions:

- Which runtime ships first, and how does the user select/configure it?
- Does Groundhog launch an agent subprocess or hand off to an interactive session?
- How does a direct-API option authenticate and control execution limits?
- How does a runtime request clarification in a terminal or unattended invocation?
- How are generated files and verification results communicated back to Groundhog?
- What resume behavior is supported after cancellation or provider failure?
- Does app intent come from the prompt alone or need an explicit future option?
- How should an invocation from inside an existing coding-agent session behave?
- What does `--offline` mean for an explicitly requested remote-model operation,
  given its current narrower toolchain meaning?

### Prompt phase 2: ship one supported workflow

Implement the selected runtime, context handoff, progress and cancellation,
failure reporting, and verification. Cover orchestration with a controlled test
runtime, then run documented live trials for a simple contract and an application.
Live model outputs need not be byte-identical; require the same project and
verification contracts. Do not make model availability a dependency of template CI.

## 8. Delivery plan

### A. Minimal default and template catalog

- [ ] Introduce the shared base scaffold and catalog.
- [ ] Make the no-option command generate the minimal contract.
- [ ] Retain `counter` as an explicit template; remove public `stub` selection.
- [ ] Add `templates list` and `templates show`, with typed JSON reports.
- [ ] Update creation output, help, examples, release notes, and agent knowledge.
- [ ] Test generation, name validation, existing destinations, unknown IDs,
      deprecated `stub` guidance, and JSON contracts.

Acceptance: the base and counter projects build and pass their contract tests.
Discovery matches the templates actually available. During this phase, list
only `counter`; add `counter-app` when it is functional.

### B. Counter application

- [ ] Prove the public TS dependency set, root layout, browser wallet path, and RPC access.
- [ ] Confirm the frontend stack, Node version, and package-manager commands.
- [ ] Implement app assets, bindings workflow, deployment configuration, and UI.
- [ ] Implement the local startup and shutdown workflow.
- [ ] Add the clean-chain TypeScript and browser end-to-end test.
- [ ] Register `counter-app` and document prerequisites and reset behavior.

Acceptance: a fresh generated project reaches a working browser counter through
the documented commands, with no manual source or address changes. The automated
end-to-end test and restart check pass.

### C. Prompt execution research

- [ ] Compare API-key integration with installed-agent execution.
- [ ] Prototype the selected path and record the decision and remaining limits.
- [ ] Specify configuration, interaction, cancellation, JSON, and verification behavior.

Acceptance: one demonstrated execution path and an implementable interface
decision. This phase can run alongside A and B.

### D. Prompt implementation

- [ ] Add `--prompt` validation and the chosen runtime integration.
- [ ] Reuse the base scaffold, catalog, and version-matched agent guidance.
- [ ] Implement result reporting and interrupted/failed-run recovery guidance.
- [ ] Test orchestration and run live contract/application trials.

Acceptance: a prompt produces a project with explicit verification results;
failed or incomplete work is not reported as verified success. D depends on C
and the scaffold foundation; application trials also depend on B.

## 9. Release checks and maintenance

Run the appropriate CLI and agent-knowledge tests, formatting, and lint checks.
Check packaged assets with `cargo package -p groundhog-cli --list --allow-dirty`.
Run generated-project checks against the dependency versions users can install,
including any registry authentication required by those releases.

Treat each official template as a supported example. Dependency upgrades must
re-run its build and test workflow. Update the embedded Groundhog knowledge when
the CLI contract or generated project changes. Move completed implementation
planning into the archive once the current behavior is covered by product docs.

## 10. Design references

- [Forge project setup](https://getfoundry.sh/projects/creating-a-new-project/):
  small standard layout and repository-based template extension.
- [Forge init](https://getfoundry.sh/forge/reference/forge-init/): explicit empty
  project option and template selection.
- [Anchor CLI](https://www.anchor-lang.com/docs/references/cli): integrated program,
  client, and local test workflow. An application directory alone is not a
  complete frontend; Groundhog's `counter-app` must connect the full workflow.

Use these as workflow references. Groundhog's public APIs and programming model
remain authoritative for generated projects.
