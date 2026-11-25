// cliff-veil vesting mini-app (RFP-017 Usability 2, 4, 5; Privacy 2).
//
// Recipient view: positions, total locked, claimable now, next unlock, claim with a pre-claim summary, gas check and,
// for a claim to a private account, the privacy disclosure that must be acknowledged before the claim is sent.
// Creator view: create with every parameter, list active schedules, cancel, signal a milestone.
//
// Every number and every rule comes from the core module `cliff_veil_module` (Rust), which reaches the SDK through
// the cliff-veil command line. This file only lays the answers out; the module refuses a private claim that was not
// acknowledged even if a button here were enabled by mistake.
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

// A Pane, so the view paints the host theme's window background behind its text.
Pane {
    id: root
    width: 760
    height: 720
    padding: 0

    readonly property string moduleName: "cliff_veil_module"
    property string lastError: ""

    // Calls a core-module method; always returns an object ({ok: false, error} on any failure).
    function call(method, args) {
        if (typeof logos === "undefined")
            return { ok: false, error: { code: "no_host", message: "the Logos host bridge is not available" } }
        try {
            var r = logos.callModule(root.moduleName, method, args)
            var v = (typeof r === "string") ? JSON.parse(r) : r
            // Basecamp 0.3.2's bridge hands a string result over JSON-encoded once more ("\"{...}\""): unwrap it.
            if (typeof v === "string")
                v = JSON.parse(v)
            if (v === null || v === undefined)
                return { ok: false, error: { code: "no_answer", message: "the core module did not answer (is cliff_veil_module installed and loaded?)" } }
            return v
        } catch (e) {
            return { ok: false, error: { code: "call_failed", message: String(e) } }
        }
    }

    function errorText(r) {
        if (r.errors)
            return r.errors.map(function (e) { return e.field + ": " + e.message + (e.program_code ? " (code " + e.program_code + ")" : "") }).join("\n")
        if (!r.error)
            return ""
        // The host bridge answers a failed call with {"error": "<text>"}, not the module's error object.
        if (typeof r.error === "string")
            return r.error
        var t = r.error.message || r.error.code || JSON.stringify(r.error)
        if (r.error.program_code)
            t += " (program error " + r.error.program_code + ")"
        if (r.error.action)
            t += "\nWhat to do: " + r.error.action
        return t
    }

    component Field: ColumnLayout {
        property alias label: lbl.text
        property alias text: input.text
        property alias placeholder: input.placeholderText
        Layout.fillWidth: true
        spacing: 2
        Label { id: lbl; font.pixelSize: 11; color: "#555" }
        TextField { id: input; Layout.fillWidth: true; selectByMouse: true }
    }

    component Row2: RowLayout {
        property alias key: k.text
        property alias value: v.text
        Layout.fillWidth: true
        Label { id: k; Layout.preferredWidth: 190; color: "#555" }
        Label { id: v; Layout.fillWidth: true; wrapMode: Text.WrapAnywhere; font.bold: true }
    }

    component ErrorBox: Label {
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        color: "#a40000"
        visible: text !== ""
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 8

        Label { text: "cliff-veil vesting"; font.pixelSize: 20; font.bold: true }

        TabBar {
            id: tabs
            objectName: "tabs"
            Layout.fillWidth: true
            TabButton { text: "Recipient" }
            TabButton { text: "Creator" }
            TabButton { text: "Settings" }
        }

        StackLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: tabs.currentIndex

            // ------------------------------------------------------------------ Recipient
            ScrollView {
                id: recipientTab
                objectName: "recipientTab"
                clip: true
                contentWidth: availableWidth

                property var view: null
                property string schedule: ""
                property var claim: null
                property var receipt: null

                function load() {
                    var r = root.call("recipient_view", [beneficiary.text, recipientSchedules.text])
                    recipientError.text = r.ok ? "" : root.errorText(r)
                    view = r.ok ? r : null
                }

                ColumnLayout {
                    width: recipientTab.availableWidth
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        Field { id: beneficiary; objectName: "beneficiary"; label: "Beneficiary account"; placeholder: "base58 account id" }
                        Button { text: "Load positions"; Layout.alignment: Qt.AlignBottom; onClicked: recipientTab.load() }
                    }
                    Label { text: "Schedule accounts, one per line (empty: list the beneficiary's schedules)"; font.pixelSize: 11; color: "#555" }
                    TextArea { id: recipientSchedules; objectName: "recipientSchedules"; Layout.fillWidth: true; Layout.preferredHeight: 56; selectByMouse: true }
                    ErrorBox { id: recipientError }
                    Label {
                        Layout.fillWidth: true
                        wrapMode: Text.WrapAnywhere
                        color: "#8a5a00"
                        visible: text !== ""
                        text: recipientTab.view && recipientTab.view.skipped.length > 0
                              ? "Not shown: " + recipientTab.view.skipped.map(function (k) { return k.schedule + " (" + k.reason + ")" }).join("; ") : ""
                    }

                    Label {
                        visible: recipientTab.view !== null
                        text: recipientTab.view ? (recipientTab.view.totals.schedules + " schedule(s). Total locked: "
                              + recipientTab.view.totals.locked.map(function (a) { return a[1] + " " + a[0] }).join(", ")
                              + ". Claimable now: "
                              + recipientTab.view.totals.claimable_now.map(function (a) { return a[1] + " " + a[0] }).join(", ")) : ""
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        font.bold: true
                    }

                    Repeater {
                        model: recipientTab.view ? recipientTab.view.positions : []
                        delegate: Frame {
                            Layout.fillWidth: true
                            ColumnLayout {
                                anchors.fill: parent
                                Row2 { key: "Schedule"; value: modelData.schedule }
                                Row2 { key: "Asset / standing"; value: modelData.asset + " / " + modelData.standing }
                                Row2 { key: "Total"; value: modelData.total }
                                Row2 { key: "Vested"; value: modelData.vested + " (" + modelData.vested_share + ")" }
                                ProgressBar { Layout.fillWidth: true; from: 0; to: 10000; value: modelData.vested_bps }
                                Row2 { key: "Claimed"; value: modelData.claimed }
                                Row2 { key: "Total locked"; value: modelData.locked }
                                Row2 { key: "Claimable now"; value: modelData.claimable_now }
                                Row2 { key: "Next unlock"; value: modelData.next_unlock }
                                Row2 { key: "Terms"; value: modelData.terms.join("\n") }
                                Row2 { key: "Flags"; value: modelData.flags }
                                Row2 { key: "As of (last block)"; value: modelData.as_of }
                                RowLayout {
                                    Button {
                                        text: "Claim..."
                                        enabled: modelData.can_claim
                                        onClicked: { recipientTab.schedule = modelData.schedule; recipientTab.claim = null; recipientTab.receipt = null }
                                    }
                                    Label { text: modelData.claim_hint || ""; color: "#555"; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                                }
                            }
                        }
                    }

                    // ---- claim: request, summary, gas check, disclosure, confirm
                    Frame {
                        Layout.fillWidth: true
                        visible: recipientTab.schedule !== ""
                        ColumnLayout {
                            anchors.fill: parent
                            Label { text: "Claim from " + recipientTab.schedule; font.bold: true; wrapMode: Text.WrapAnywhere; Layout.fillWidth: true }
                            RowLayout {
                                RadioButton { id: toPublic; text: "To a public account"; checked: true }
                                RadioButton { id: toPrivate; objectName: "toPrivate"; text: "To a private account of this wallet" }
                            }
                            Field { id: publicTo; visible: toPublic.checked; label: "Public destination (empty: the beneficiary)" }
                            Field { id: privateAccount; visible: toPrivate.checked; label: "Private destination (empty: a new private account)" }
                            Field { id: feePayer; visible: toPrivate.checked; label: "Private fee payer (empty: the SDK picks the private account of this wallet, other than the destination, with the smallest balance that covers the fee reserve; if none does, the destination pays out of the claim)" }
                            Button {
                                text: "Review claim"
                                onClicked: {
                                    var req = { schedule: recipientTab.schedule, beneficiary: beneficiary.text.trim() }
                                    if (toPrivate.checked) {
                                        req.path = "private"
                                        if (privateAccount.text.trim() !== "") req.account = privateAccount.text.trim()
                                        if (feePayer.text.trim() !== "") req.fee_payer = feePayer.text.trim()
                                    } else {
                                        req.path = "public"
                                        if (publicTo.text.trim() !== "") req.to = publicTo.text.trim()
                                    }
                                    var r = root.call("prepare_claim", [JSON.stringify(req)])
                                    claimError.text = r.ok ? "" : root.errorText(r)
                                    recipientTab.claim = r.ok ? r : null
                                    recipientTab.receipt = null
                                    ackBox.checked = false
                                }
                            }
                            ErrorBox { id: claimError }

                            ColumnLayout {
                                visible: recipientTab.claim !== null
                                Layout.fillWidth: true
                                Label { text: "Pre-claim summary"; font.bold: true }
                                Repeater {
                                    model: recipientTab.claim ? recipientTab.claim.summary.lines : []
                                    delegate: Row2 { key: modelData.label; value: modelData.value }
                                }
                                Label {
                                    Layout.fillWidth: true
                                    wrapMode: Text.WordWrap
                                    property var gas: recipientTab.claim ? recipientTab.claim.summary.gas : null
                                    color: gas && gas.status === "sufficient" ? "#1a6b1a" : "#a40000"
                                    text: !gas ? "" : (gas.status === "sufficient"
                                          ? "Gas check passed: " + gas.payer + " will hold " + gas.holds + ", enough for the fee reserve of " + gas.reserve + " (estimated fee about " + gas.fee + ")."
                                          : "Gas check failed: " + gas.payer + " holds " + gas.have + ", the fee reserve is " + gas.need + ".\nWhat to do: " + gas.action)
                                }
                                Label {
                                    visible: recipientTab.claim !== null && recipientTab.claim.summary.dev_mode_note !== null
                                    text: recipientTab.claim && recipientTab.claim.summary.dev_mode_note ? recipientTab.claim.summary.dev_mode_note : ""
                                    color: "#8a5a00"; wrapMode: Text.WordWrap; Layout.fillWidth: true
                                }

                                // Privacy disclosure: private path only, shown before every claim.
                                Frame {
                                    Layout.fillWidth: true
                                    visible: recipientTab.claim !== null && recipientTab.claim.disclosure !== null
                                    ColumnLayout {
                                        anchors.fill: parent
                                        Label { text: "Privacy disclosure (read before claiming)"; font.bold: true }
                                        TextArea {
                                            Layout.fillWidth: true
                                            readOnly: true
                                            wrapMode: Text.WordWrap
                                            font.family: "monospace"
                                            font.pixelSize: 10
                                            text: recipientTab.claim && recipientTab.claim.disclosure ? recipientTab.claim.disclosure.text : ""
                                        }
                                        CheckBox {
                                            id: ackBox
                                            objectName: "ackBox"
                                            text: "I have read what this claim makes public and what it does not"
                                            onToggled: {
                                                if (!checked || !recipientTab.claim) return
                                                var r = root.call("acknowledge_disclosure", [recipientTab.claim.flow, recipientTab.claim.disclosure.digest])
                                                claimError.text = r.ok ? "" : root.errorText(r)
                                                if (r.ok) recipientTab.claim = r; else checked = false
                                            }
                                        }
                                    }
                                }

                                Label {
                                    visible: recipientTab.claim !== null && recipientTab.claim.blocked !== null
                                    text: recipientTab.claim && recipientTab.claim.blocked ? "Not ready: " + recipientTab.claim.blocked.message : ""
                                    color: "#555"; wrapMode: Text.WordWrap; Layout.fillWidth: true
                                }
                                Button {
                                    text: "Confirm claim"
                                    enabled: recipientTab.claim !== null && recipientTab.claim.stage === "ready"
                                    onClicked: {
                                        var r = root.call("confirm_claim", [recipientTab.claim.flow])
                                        claimError.text = r.ok ? "" : root.errorText(r)
                                        recipientTab.receipt = r.ok ? r.receipt : null
                                        recipientTab.claim = null
                                        if (r.ok) recipientTab.load()
                                    }
                                }
                            }
                            Label {
                                visible: recipientTab.receipt !== null
                                text: recipientTab.receipt ? "Claimed: transaction " + recipientTab.receipt.hash + " in block " + recipientTab.receipt.block : ""
                                color: "#1a6b1a"; wrapMode: Text.WrapAnywhere; Layout.fillWidth: true
                            }
                        }
                    }
                }
            }

            // ------------------------------------------------------------------ Creator
            ScrollView {
                id: creatorTab
                objectName: "creatorTab"
                clip: true
                contentWidth: availableWidth

                property var rows: []
                property string skippedText: ""
                property var op: null
                property string opTitle: ""

                function list() {
                    var r = root.call("creator_view", [creator.text, creatorSchedules.text])
                    listError.text = r.ok ? "" : root.errorText(r)
                    rows = r.ok ? r.schedules : []
                    skippedText = r.ok && r.skipped.length > 0
                                  ? "Not shown: " + r.skipped.map(function (k) { return k.schedule + " (" + k.reason + ")" }).join("; ") : ""
                }
                function form() {
                    return {
                        creator: creator.text, beneficiary: cBeneficiary.text,
                        asset: assetBox.currentText, token_definition: tokenDef.text,
                        kind: kindBox.currentText, total: total.text,
                        start: start.text, cliff: cliff.text, end: end.text, milestones: milestones.text,
                        cancelable: cancelable.checked, transferable: transferable.checked,
                        cancel_authority: cancelAuth.text, milestone_authority: milestoneAuth.text,
                        refund_to: refundTo.text, label: label.text
                    }
                }
                function prepared(title, r) {
                    opError.text = r.ok ? "" : root.errorText(r)
                    op = r.ok ? r : null
                    opTitle = title
                    opDone.text = ""
                }

                ColumnLayout {
                    width: creatorTab.availableWidth
                    spacing: 8

                    Field { id: creator; objectName: "creator"; label: "Creator account (signs, funds the escrow)"; placeholder: "base58 account id" }

                    Label { text: "Create a schedule"; font.bold: true; font.pixelSize: 15 }
                    Field { id: cBeneficiary; label: "Beneficiary" }
                    RowLayout {
                        ColumnLayout { Label { text: "Asset"; font.pixelSize: 11 } ComboBox { id: assetBox; model: ["native", "token"] } }
                        Field { id: tokenDef; visible: assetBox.currentText === "token"; label: "Token definition account" }
                    }
                    RowLayout {
                        ColumnLayout { Label { text: "Kind"; font.pixelSize: 11 } ComboBox { id: kindBox; model: ["linear", "milestones"] } }
                        Field { id: total; label: kindBox.currentText === "linear" ? "Total (base units)" : "Total (optional: the sum of the milestones)" }
                    }
                    RowLayout {
                        visible: kindBox.currentText === "linear"
                        Field { id: start; label: "Start (UTC, e.g. 2026-12-01T00:00Z, or ms)" }
                        Field { id: cliff; label: "Cliff (= start: fully linear)" }
                        Field { id: end; label: "End (= cliff: single cliff)" }
                    }
                    Field { id: milestones; visible: kindBox.currentText === "milestones"; label: "Milestone amounts, separated by ;"; placeholder: "100; 200; 300" }
                    RowLayout {
                        CheckBox { id: cancelable; text: "Cancelable"; checked: true }
                        CheckBox { id: transferable; text: "Transferable"; checked: false }
                    }
                    Field { id: cancelAuth; label: "Cancel authority (empty: the creator)" }
                    Field { id: milestoneAuth; label: "Milestone authority (empty: the creator)" }
                    Field { id: refundTo; label: "Refund account (empty: the creator)" }
                    Field { id: label; label: "Label for the schedule id (empty: random id)" }
                    Button { text: "Review creation"; onClicked: creatorTab.prepared("Create", root.call("prepare_create", [JSON.stringify(creatorTab.form())])) }

                    Label { text: "Active schedules"; font.bold: true; font.pixelSize: 15 }
                    Label { text: "Schedule accounts, one per line (empty: list the creator's schedules)"; font.pixelSize: 11; color: "#555" }
                    TextArea { id: creatorSchedules; objectName: "creatorSchedules"; Layout.fillWidth: true; Layout.preferredHeight: 56; selectByMouse: true }
                    Button { text: "Load schedules"; onClicked: creatorTab.list() }
                    ErrorBox { id: listError }
                    Label { text: creatorTab.skippedText; color: "#8a5a00"; wrapMode: Text.WrapAnywhere; Layout.fillWidth: true; visible: text !== "" }
                    Repeater {
                        model: creatorTab.rows
                        delegate: Frame {
                            Layout.fillWidth: true
                            ColumnLayout {
                                anchors.fill: parent
                                Row2 { key: "Schedule"; value: modelData.schedule }
                                Row2 { key: "Beneficiary"; value: modelData.beneficiary }
                                Row2 { key: "Asset / standing"; value: modelData.asset + " / " + modelData.standing }
                                Row2 { key: "Total / claimed"; value: modelData.total + " / " + modelData.claimed }
                                Row2 { key: "Locked / claimable now"; value: modelData.locked + " / " + modelData.claimable_now }
                                Row2 { key: "Terms"; value: modelData.terms.join("\n") }
                                RowLayout {
                                    Button {
                                        text: "Cancel schedule..."
                                        enabled: modelData.can_cancel
                                        onClicked: creatorTab.prepared("Cancel " + modelData.schedule, root.call("prepare_cancel", [modelData.schedule, creator.text]))
                                    }
                                    Label { text: modelData.cancel_hint || ""; color: "#555" }
                                }
                                RowLayout {
                                    ComboBox { id: milestoneIndex; model: modelData.signal_indices; enabled: modelData.signal_indices.length > 0 }
                                    Button {
                                        text: "Signal milestone..."
                                        enabled: modelData.signal_indices.length > 0
                                        onClicked: creatorTab.prepared("Signal milestone " + milestoneIndex.currentText + " of " + modelData.schedule,
                                                                       root.call("prepare_signal_request", [JSON.stringify({ schedule: modelData.schedule, authority: creator.text, index: Number(milestoneIndex.currentText) })]))
                                    }
                                    Label { text: modelData.signal_hint || ""; color: "#555" }
                                }
                            }
                        }
                    }

                    // ---- confirmation of a creation, cancel or signal
                    ErrorBox { id: opError }
                    Frame {
                        Layout.fillWidth: true
                        visible: creatorTab.op !== null
                        ColumnLayout {
                            anchors.fill: parent
                            Label { text: "Review: " + creatorTab.opTitle; font.bold: true; wrapMode: Text.WrapAnywhere; Layout.fillWidth: true }
                            Repeater {
                                model: creatorTab.op && creatorTab.op.lines ? creatorTab.op.lines : []
                                delegate: Row2 { key: modelData[0]; value: modelData[1] }
                            }
                            Label { text: "Predicted outcome (dry run)"; font.bold: true }
                            Repeater {
                                model: creatorTab.op ? creatorTab.op.outcome_lines : []
                                delegate: Row2 { key: modelData[0]; value: modelData[1] }
                            }
                            Row2 { key: "Estimated network fee"; value: creatorTab.op ? "~" + creatorTab.op.fee + ", paid by " + creatorTab.op.fee_payer : "" }
                            Button {
                                text: "Confirm and send"
                                onClicked: {
                                    var r = root.call("confirm_op", [creatorTab.op.op])
                                    opError.text = r.ok ? "" : root.errorText(r)
                                    opDone.text = r.ok ? "Sent: transaction " + r.receipt.hash + " in block " + r.receipt.block : ""
                                    creatorTab.op = null
                                    if (r.ok) creatorTab.list()
                                }
                            }
                        }
                    }
                    Label { id: opDone; color: "#1a6b1a"; wrapMode: Text.WrapAnywhere; Layout.fillWidth: true; visible: text !== "" }
                }
            }

            // ------------------------------------------------------------------ Settings
            ColumnLayout {
                spacing: 8
                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: "The core module runs the cliff-veil command line (the SDK's front end) for every request: with --dry-run to review, with --yes to send. Give its path and its environment: the wallet home and password, the program ids file (CV_IDS) or the vesting program id (CV_VESTING), and RISC0_DEV_MODE=1 for dev mode."
                }
                Field { id: cliPath; label: "Path of the cliff-veil command line"; placeholder: "/path/to/cliff-veil" }
                Label { text: "Environment (JSON object)"; font.pixelSize: 11; color: "#555" }
                TextArea {
                    id: envJson
                    Layout.fillWidth: true
                    Layout.preferredHeight: 120
                    font.family: "monospace"
                    text: "{\n  \"LEE_WALLET_HOME_DIR\": \"\",\n  \"LEE_WALLET_PASSWORD\": \"\",\n  \"CV_IDS\": \"\",\n  \"RISC0_DEV_MODE\": \"1\"\n}"
                }
                Button {
                    text: "Apply"
                    onClicked: {
                        var env
                        try { env = JSON.parse(envJson.text) } catch (e) { settingsMsg.text = "The environment is not a JSON object: " + e; return }
                        var r = root.call("configure", [JSON.stringify({ cli: cliPath.text, env: env })])
                        settingsMsg.text = r.ok ? "Applied: " + r.cli : root.errorText(r)
                    }
                }
                Button {
                    text: "Check: read the privacy disclosure through the command line"
                    onClicked: {
                        var r = root.call("disclosure", [])
                        settingsMsg.text = r.ok ? "The command line answers; its disclosure names every required item." : root.errorText(r)
                    }
                }
                Label { id: settingsMsg; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                Item { Layout.fillHeight: true }
            }
        }
    }
}
