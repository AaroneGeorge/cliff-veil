/*
 * claim.c: the cliff-veil C ABI from C, against the in-memory chain (no sequencer needed).
 *
 * Creates a cliff + linear schedule, reads it, claims once to a public account and once to a private account (the
 * private claim is refused until its privacy disclosure is acknowledged), then cancels it; the vested remainder stays
 * claimable. Exits 0 when every step
 * answered as expected.
 *
 *   make c-example        (from the repository root; builds the library, compiles and runs this file)
 *
 * A Logos core module makes the same calls; it reads the answers with its JSON parser (QJsonDocument) instead of
 * the small helpers below.
 */
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cliff_veil.h"

#define T0 1767225600000ULL /* 2026-01-01T00:00:00Z */
#define DAY 86400000ULL

static int failures = 0;

/* The text of string field `key` in `json` (first occurrence), copied into `out`. Enough for these answers. */
static int get_string(const char *json, const char *key, char *out, size_t size) {
    char pattern[64];
    snprintf(pattern, sizeof pattern, "\"%s\":\"", key);
    const char *start = strstr(json, pattern);
    if (!start) return 0;
    start += strlen(pattern);
    const char *end = strchr(start, '"');
    if (!end || (size_t)(end - start) >= size) return 0;
    memcpy(out, start, (size_t)(end - start));
    out[end - start] = '\0';
    return 1;
}

/* The first string of array field `key` (first occurrence). */
static int get_first(const char *json, const char *key, char *out, size_t size) {
    char pattern[64];
    snprintf(pattern, sizeof pattern, "\"%s\":[\"", key);
    const char *start = strstr(json, pattern);
    if (!start) return 0;
    start += strlen(pattern);
    size_t n = strcspn(start, "\"");
    if (n >= size) return 0;
    memcpy(out, start, n);
    out[n] = '\0';
    return 1;
}

/* Number field `key` (first occurrence). */
static uint64_t get_u64(const char *json, const char *key) {
    char pattern[64];
    snprintf(pattern, sizeof pattern, "\"%s\":", key);
    const char *start = strstr(json, pattern);
    return start ? strtoull(start + strlen(pattern), NULL, 10) : 0;
}

/* Prints a step, checks whether it succeeded as expected, and frees the answer into `keep` (if given). */
static void step(const char *what, char *answer, int expect_ok, char *keep, size_t keep_size) {
    int ok = strstr(answer, "\"ok\":true") != NULL;
    int good = ok == expect_ok;
    printf("%s %s\n    %.300s%s\n", good ? "[pass]" : "[FAIL]", what, answer, strlen(answer) > 300 ? " ..." : "");
    if (!good) failures++;
    if (keep) {
        strncpy(keep, answer, keep_size - 1);
        keep[keep_size - 1] = '\0';
    }
    cv_string_free(answer);
}

int main(void) {
    static char buf[16384];
    char request[1024], schedule[128], ack[80];
    CvClient *client = NULL;

    printf("cliff-veil C ABI version %" PRIu32 " (header %d)\n", cv_abi_version(), CV_ABI_VERSION);
    if (cv_abi_version() != CV_ABI_VERSION) return 2;

    snprintf(request, sizeof request,
             "{\"backend\":\"memory\",\"now_ms\":%llu,"
             "\"public\":[{\"label\":\"creator\",\"balance\":\"1000000\"},{\"label\":\"alice\",\"balance\":\"1000\"}],"
             "\"private\":[{\"label\":\"vault\",\"balance\":\"0\"}]}",
             T0);
    step("open an in-memory client", cv_client_open(request, &client), 1, NULL, 0);
    if (!client) return 1;

    /* Create: 100 000 over 100 days, 10-day cliff. Prepare (checked, priced), then send. */
    snprintf(request, sizeof request,
             "{\"from\":\"creator\",\"beneficiary\":\"alice\",\"total\":\"100000\",\"start\":%llu,"
             "\"cliff\":%llu,\"end\":%llu,\"label\":\"c-example\"}",
             T0, T0 + 10 * DAY, T0 + 100 * DAY);
    step("prepare create", cv_prepare_create(client, request), 1, buf, sizeof buf);
    if (!get_first(buf, "schedules", schedule, sizeof schedule)) return 1;
    step("send create", cv_send_prepared(client, get_u64(buf, "prepared"), NULL), 1, NULL, 0);

    step("claimable before the cliff (nothing)", cv_claimable(client, schedule), 1, NULL, 0);
    snprintf(request, sizeof request, "{\"schedule\":\"%s\",\"as\":\"alice\"}", schedule);
    step("claim before the cliff is refused (code 14)", cv_prepare_claim(client, request), 0, NULL, 0);

    /* Day 50: half vested. Public claim. */
    step("set the clock to day 50", cv_memory_set_clock(client, T0 + 50 * DAY), 1, NULL, 0);
    step("prepare a public claim (summary + estimate)", cv_prepare_claim(client, request), 1, buf, sizeof buf);
    step("send the public claim", cv_send_prepared(client, get_u64(buf, "prepared"), NULL), 1, NULL, 0);

    /* Day 60: private claim to the wallet's private account `vault`. */
    step("set the clock to day 60", cv_memory_set_clock(client, T0 + 60 * DAY), 1, NULL, 0);
    snprintf(request, sizeof request,
             "{\"schedule\":\"%s\",\"as\":\"alice\",\"private\":true,\"private_to\":\"vault\"}", schedule);
    step("prepare a private claim (summary + disclosure)", cv_prepare_claim(client, request), 1, buf, sizeof buf);
    uint64_t id = get_u64(buf, "prepared");
    if (!get_string(buf, "disclosure_ack", ack, sizeof ack)) return 1;
    step("send without acknowledging the disclosure is refused", cv_send_prepared(client, id, NULL), 0, NULL, 0);
    /* Here a module shows "privacy_disclosure" and waits for the person to accept it. */
    step("send after the disclosure is acknowledged", cv_send_prepared(client, id, ack), 1, NULL, 0);

    /* Day 70: cancel. 70 000 vested (10 000 of it unclaimed, stays claimable); 30 000 is refunded. */
    step("set the clock to day 70", cv_memory_set_clock(client, T0 + 70 * DAY), 1, NULL, 0);
    snprintf(request, sizeof request, "{\"schedule\":\"%s\",\"as\":\"creator\"}", schedule);
    step("prepare cancel (refund the unvested part)", cv_prepare_cancel(client, request), 1, buf, sizeof buf);
    step("send cancel", cv_send_prepared(client, get_u64(buf, "prepared"), NULL), 1, NULL, 0);
    step("show the schedule", cv_show(client, schedule), 1, buf, sizeof buf);
    if (!strstr(buf, "\"claimed\":\"60000\"") || !strstr(buf, "\"claimable_now\":\"10000\"") ||
        !strstr(buf, "\"standing\":\"cancelled\"")) {
        printf("[FAIL] expected claimed 60000, claimable 10000, standing cancelled\n");
        failures++;
    }

    cv_client_close(client);
    printf("%s: %d failure(s)\n", failures ? "FAILED" : "all steps passed", failures);
    return failures ? 1 : 0;
}
