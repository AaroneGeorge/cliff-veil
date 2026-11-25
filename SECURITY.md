# Security policy

**No deployment of cliff-veil should hold real value until the external audit is closed (milestone M5).** The
programs hold funds in escrow and have not had a third-party audit. What the design protects, and what it does not, is
in [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).

## Supported versions

| Version | Supported |
|---|---|
| `main` | Yes |
| The latest tag | Yes |
| Older tags and commits | No: reproduce on `main` or the latest tag first |

Deployed programs are immutable. A fix ships as a new deployment at a new program id, with migration notes; it does
not change schedules already created on an older deployment.

## How to report a vulnerability

Report privately through GitHub's private vulnerability reporting on this repository: open the **Security** tab of
[github.com/AaroneGeorge/cliff-veil](https://github.com/AaroneGeorge/cliff-veil/security) and choose **Report a
vulnerability**. The report reaches the maintainers only.

Please do not open a public issue, pull request or discussion about a vulnerability. If the private form is not
available to you, open a public issue that asks for a private contact and contains no details.

A useful report names the commit or tag, the component (`vesting`, `vesting_fees`, `core/`, the SDK, the C ABI, the
command line, the mini-app, the public endpoint relay), the steps or a test that reproduces it, and the impact you
expect.

## What to expect

| Step | When |
|---|---|
| Acknowledgement | Within 48 hours |
| First assessment: confirmed or not, and a severity | Within 7 days |
| Fix or mitigation, critical (funds can be taken or locked) | As soon as possible, aimed at 7 days |
| Fix or mitigation, high | Aimed at 30 days |
| Fix or mitigation, medium or low | In the next release |

We keep you informed while we work on it, and we credit you in the advisory unless you ask us not to.

## Coordinated disclosure

We publish an advisory when a fix or mitigation is released, or 90 days after the report, whichever comes first. We may
agree a different date with the reporter, for example when a fix needs a new deployment.

## Scope

In scope:

- the `vesting` and `vesting_fees` programs (`program/`), and the rules crate (`core/`);
- the SDK (`sdk/`), its C ABI (`sdk/ffi/`) and the command-line client (`cli/`), in particular the private-claim path
  and the disclosure gate;
- the mini-app (`miniapp/`);
- the read-only public endpoint relay (`tools/public_endpoint/`);
- the build and deployment scripts (`scripts/`), where they affect what is deployed.

Out of scope here: LEZ itself, the token and associated-token-account programs, the RFP-001 admin-authority library and
RISC Zero, unless the issue is in how cliff-veil uses them; please report those to their own projects
([docs/REUSE.md](docs/REUSE.md) lists them). Also out of scope: denial of service by request volume against the public
endpoint, and findings that need a compromised signing key, except where cliff-veil gives that key more power than
[docs/THREAT-MODEL.md](docs/THREAT-MODEL.md) states.

## Safe harbour

We will not pursue legal action against anyone who researches and reports a vulnerability in good faith under this
policy: who avoids harm to other people's funds and data, tests only on local chains or on accounts they control, and
gives us reasonable time to fix the issue before disclosing it.
