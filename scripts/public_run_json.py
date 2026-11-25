#!/usr/bin/env python3
"""Builds run.json of a public-chain run (scripts/public_lifecycle.sh) from its steps/ directory.

    python3 -I scripts/public_run_json.py evidence/public-chain/<utc> [--url PUBLIC_URL]

run.json lists every transaction (hash, block, what it is), every refusal (error code and message), and the
expected state of every schedule; scripts/verify_public.sh re-reads all of it from a sequencer endpoint.
Standard library only.
"""

import csv
import json
import os
import re
import subprocess
import sys
import tomllib


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    out = args[0]
    url = args[args.index("--url") + 1] if "--url" in args else None
    steps_dir = os.path.join(out, "steps")
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

    with open(os.path.join(repo, "deploy", "ids-local-pub.toml"), "rb") as f:
        ids = tomllib.load(f)

    transactions, refusals, queries = [], [], []

    # The deployment (deploy.log, the output of scripts/deploy_local.sh pub) comes first, if the run kept it.
    deploy_log = os.path.join(out, "deploy.log")
    if os.path.exists(deploy_log):
        program, k, pending = None, 0, None
        for line in open(deploy_log, encoding="utf-8"):
            m = re.match(r"deploying (\w+) \(.*\) header (\w+)", line)
            if m:
                program, k = m.group(1), 0
            m = re.match(r"Transaction hash is ([0-9a-f]{64})", line)
            if m:
                pending = m.group(1)
            m = re.match(r"Transaction is included in block (\d+)", line)
            if m and pending and program:
                k += 1
                transactions.append({"step": 0, "what": f"deploy {program}: program-loader transaction {k}",
                                     "operation": "program-loader deploy", "hash": pending, "block": int(m.group(1))})
                pending = None
            m = re.match(r"init: tx ([0-9a-f]{64}) included in block (\d+)", line)
            if m:
                transactions.append({"step": 0, "what": "vesting_fees Init by the deployer", "operation": "init",
                                     "hash": m.group(1), "block": int(m.group(2))})

    # Funding transfers (native, from the deployer's account).
    with open(os.path.join(steps_dir, "funding.tsv"), encoding="utf-8") as f:
        for row in csv.reader(f, delimiter="\t"):
            _, h, b, to, amount = row
            transactions.append({"step": 0, "what": f"native transfer of {amount} to {to} (account funding)",
                                 "operation": "auth-transfer", "hash": h, "block": int(b)})

    shown = {}
    with open(os.path.join(steps_dir, "index.tsv"), encoding="utf-8") as f:
        for row in csv.reader(f, delimiter="\t"):
            n, expect, rc, what, command = row
            with open(os.path.join(steps_dir, f"{n}.json"), encoding="utf-8") as g:
                doc = json.load(g)
            step = {"step": int(n), "what": what, "command": f"cliff-veil {command}"}
            if expect == "refused":
                err = doc.get("error") or {}
                refusals.append({**step, "sent": False, "exit": int(rc), "code": err.get("code"),
                                 "name": err.get("name"), "message": err.get("message")})
                continue
            sent = doc.get("sent", [])
            prepared = doc.get("transactions", [])
            for i, r in enumerate(sent):
                p = prepared[i] if i < len(prepared) else {}
                tx = {**step, "operation": p.get("operation"), "hash": r["hash"], "block": r["block"]}
                if p.get("schedules"):
                    tx["schedules"] = p["schedules"]
                if p.get("schedule"):
                    tx["schedule"] = p["schedule"]
                for k in ("amount", "unlocks", "refunded", "from", "to"):
                    if k in p:
                        tx[k] = p[k]
                if len(sent) > 1:
                    tx["part"] = f"{i + 1} of {len(sent)}"
                transactions.append(tx)
            if not sent:
                queries.append(step)
            if "schedule" in doc and isinstance(doc["schedule"], dict):
                shown[doc["schedule"]["account"]] = doc["schedule"]

    names = {}
    with open(os.path.join(steps_dir, "schedules.tsv"), encoding="utf-8") as f:
        for name, account in csv.reader(f, delimiter="\t"):
            names[account] = name

    # Expected state: the last `show` of a schedule, else (batch schedules never claimed) the terms of the batch file.
    batch = {}
    with open(os.path.join(out, "batch.csv"), encoding="utf-8") as f:
        for i, (ben, total, _label) in enumerate(csv.reader(f), start=1):
            batch[f"batch-{i:02d}"] = {"beneficiary": ben, "total": total}
    fields = ("schedule_id", "creator", "beneficiary", "asset", "token_definition", "kind_detail", "total", "claimed",
              "cancelable", "transferable", "cancel_authority", "milestone_authority", "refund_to", "cancelled",
              "standing")
    schedules = []
    for account, name in names.items():
        if account in shown:
            expect = {k: shown[account][k] for k in fields if k in shown[account]}
            source = "last show of this run"
        else:
            expect = {**batch[name], "claimed": "0", "asset": "native", "cancelled": None}
            source = "batch file"
        schedules.append({"name": name, "account": account, "expect": expect, "expect_from": source})

    head = open(os.path.join(out, "run.log"), encoding="utf-8").read().splitlines()[:3]
    commit = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    run = {
        "what": "cliff-veil public lifecycle on local-pub: a standalone LEZ v0.4.0-rc3 sequencer, RISC0_DEV_MODE unset",
        "run": os.path.basename(os.path.normpath(out)),
        "header": head,
        "repository_commit": commit,
        "network": ids["deploy"]["network"],
        "lez": ids["deploy"]["lez"],
        "dev_mode": ids["deploy"]["dev_mode"],
        "public_url": url,
        "programs": ids["programs"],
        "image_ids": ids.get("image_ids", {}),
        "accounts": ids.get("accounts", {}),
        "transactions": transactions,
        "refusals": refusals,
        "queries": queries,
        "schedules": schedules,
    }
    with open(os.path.join(out, "run.json"), "w", encoding="utf-8") as f:
        json.dump(run, f, indent=2)
        f.write("\n")
    print(f"run.json: {len(transactions)} transactions, {len(refusals)} refusals, {len(queries)} queries, "
          f"{len(schedules)} schedules")


if __name__ == "__main__":
    main()
