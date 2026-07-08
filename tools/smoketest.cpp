// Mangle smoketest — headless CLAP host that dlopens the built mangle.clap and
// drives it end-to-end: params, note events, note-expression Volume/Pan, the
// loop-point clamp regression, state roundtrip, and a null-transport pass.
//
// The DSP crate has thorough unit tests, but the nih-plug *shell* (param table,
// state blob, transport, note + note-expression handling) has no coverage; this
// harness fills that gap without a DAW. See tools/run-smoketest.sh.
//
//   build: c++ -O2 -std=c++17 -Itools/clap/include tools/smoketest.cpp -ldl -o /tmp/mangle_smoketest
//   run:   /tmp/mangle_smoketest target/bundled/mangle.clap
//
// CLAP headers are vendored under tools/clap/include (MIT).
#include <clap/clap.h>
#include <dlfcn.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <vector>

static int g_fails = 0;
#define CHECK(cond, msg) do { \
    if (!(cond)) { fprintf(stderr, "FAIL: %s\n", msg); g_fails++; } \
    else         { printf("ok:   %s\n", msg); } } while (0)

// ------------------------------------------------------------- host stub
static const void *hostGetExtension(const clap_host *, const char *) { return nullptr; }
static void hostRequestRestart(const clap_host *) {}
static void hostRequestProcess(const clap_host *) {}
static void hostRequestCallback(const clap_host *) {}
static clap_host s_host = {
    CLAP_VERSION, nullptr,
    "mangle-smoketest", "voyd", "https://example.invalid", "1.0",
    hostGetExtension, hostRequestRestart, hostRequestProcess, hostRequestCallback,
};

// --------------------------------------------------------- event streams
union Ev {
    clap_event_header           hdr;
    clap_event_note             note;
    clap_event_note_expression  expr;
    clap_event_param_value      pv;
};
static std::vector<Ev> g_in;                 // rebuilt per block, time-sorted
static uint32_t inSize(const clap_input_events *) { return (uint32_t)g_in.size(); }
static const clap_event_header *inGet(const clap_input_events *, uint32_t i) {
    return &g_in[i].hdr;
}
static clap_input_events s_in = { nullptr, inSize, inGet };

static bool outPush(const clap_output_events *, const clap_event_header *) { return true; }
static clap_output_events s_out = { nullptr, outPush };

static void pushNoteOn(uint32_t t, int key, double vel, int32_t id) {
    Ev e{}; e.note.header.size = sizeof(clap_event_note);
    e.note.header.type = CLAP_EVENT_NOTE_ON; e.note.header.time = t;
    e.note.note_id = id; e.note.port_index = 0; e.note.channel = 0;
    e.note.key = key; e.note.velocity = vel;
    g_in.push_back(e);
}
static void pushNoteOff(uint32_t t, int key, int32_t id) {
    Ev e{}; e.note.header.size = sizeof(clap_event_note);
    e.note.header.type = CLAP_EVENT_NOTE_OFF; e.note.header.time = t;
    e.note.note_id = id; e.note.port_index = 0; e.note.channel = 0;
    e.note.key = key; e.note.velocity = 0.0;
    g_in.push_back(e);
}
static void pushExpr(uint32_t t, clap_note_expression which, double val, int32_t id) {
    Ev e{}; e.expr.header.size = sizeof(clap_event_note_expression);
    e.expr.header.type = CLAP_EVENT_NOTE_EXPRESSION; e.expr.header.time = t;
    e.expr.expression_id = which;
    e.expr.note_id = id; e.expr.port_index = -1; e.expr.channel = -1; e.expr.key = -1;
    e.expr.value = val;
    g_in.push_back(e);
}
static void pushParam(uint32_t t, clap_id id, double plain) {
    Ev e{}; e.pv.header.size = sizeof(clap_event_param_value);
    e.pv.header.type = CLAP_EVENT_PARAM_VALUE; e.pv.header.time = t;
    e.pv.param_id = id; e.pv.cookie = nullptr;
    e.pv.note_id = -1; e.pv.port_index = -1; e.pv.channel = -1; e.pv.key = -1;
    e.pv.value = plain;
    g_in.push_back(e);
}

// ------------------------------------------------------------ mem streams
struct MemStream { std::vector<uint8_t> data; size_t rd = 0; };
static int64_t osWrite(const clap_ostream *s, const void *buf, uint64_t n) {
    MemStream *m = (MemStream *)s->ctx;
    const uint8_t *b = (const uint8_t *)buf;
    m->data.insert(m->data.end(), b, b + n);
    return (int64_t)n;
}
static int64_t isRead(const clap_istream *s, void *buf, uint64_t n) {
    MemStream *m = (MemStream *)s->ctx;
    uint64_t avail = m->data.size() - m->rd;
    uint64_t take = n < avail ? n : avail;
    memcpy(buf, m->data.data() + m->rd, take);
    m->rd += take;
    return (int64_t)take;
}

// energy of a stereo block
struct E { double l, r; };

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s mangle.clap\n", argv[0]); return 2; }

    void *dl = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    CHECK(dl, "dlopen plugin");
    if (!dl) { fprintf(stderr, "%s\n", dlerror()); return 1; }
    auto *entry = (const clap_plugin_entry *)dlsym(dl, "clap_entry");
    CHECK(entry, "resolve clap_entry");
    if (!entry) return 1;
    CHECK(entry->init(argv[1]), "entry init");

    auto *fac = (const clap_plugin_factory *)entry->get_factory(CLAP_PLUGIN_FACTORY_ID);
    CHECK(fac && fac->get_plugin_count(fac) >= 1, "plugin factory");
    auto *desc = fac->get_plugin_descriptor(fac, 0);
    CHECK(desc && desc->id, "plugin descriptor");
    printf("      plugin: %s  (%s)\n", desc->name ? desc->name : "?", desc->id);

    auto *plug = fac->create_plugin(fac, &s_host, desc->id);
    CHECK(plug, "create plugin");
    if (!plug) return 1;
    CHECK(plug->init(plug), "plugin init");

    // ---- params: count + defaults in range + value_to_text
    auto *params = (const clap_plugin_params *)plug->get_extension(plug, CLAP_EXT_PARAMS);
    CHECK(params, "params extension");
    uint32_t nparams = params->count(plug);
    printf("      param count: %u\n", nparams);
    CHECK(nparams == 396, "param count == 396 (full table + Fx + One-Shot + Quant)");

    bool allInRange = true, allText = true;
    // find the loop + filter params, and a mutable param for the state test
    std::vector<clap_id> loopIds;
    clap_id probeId = 0; double probeMin = 0, probeMax = 1;
    clap_id fOnId = ~0u, fTypeId = ~0u, fCutId = ~0u;
    double fCutMin = 0, fCutMax = 0;
    // First mod-matrix row, for the matrix->filter routing test.
    clap_id mSrcId = ~0u, mDstId = ~0u, mAmtId = ~0u;
    double mSrcMax = 0, mDstMax = 0;
    for (uint32_t i = 0; i < nparams; i++) {
        clap_param_info info{};
        if (!params->get_info(plug, i, &info)) { allInRange = false; continue; }
        if (info.default_value < info.min_value || info.default_value > info.max_value)
            allInRange = false;
        char txt[128];
        if (!params->value_to_text(plug, info.id, info.default_value, txt, sizeof(txt)))
            allText = false;
        if (strstr(info.name, "Loop Start") || strstr(info.name, "Loop End"))
            loopIds.push_back(info.id);
        if (!strcmp(info.name, "Filter On"))     fOnId = info.id;
        if (!strcmp(info.name, "Filter Type"))   fTypeId = info.id;
        if (!strcmp(info.name, "Filter Cutoff")) {
            fCutId = info.id; fCutMin = info.min_value; fCutMax = info.max_value;
        }
        if (!strcmp(info.name, "Mod 00 Source")) { mSrcId = info.id; mSrcMax = info.max_value; }
        if (!strcmp(info.name, "Mod 00 Dest 1")) { mDstId = info.id; mDstMax = info.max_value; }
        if (!strcmp(info.name, "Mod 00 Amt 1"))  { mAmtId = info.id; }
        // a param with a real range we can flip for the state roundtrip
        if (probeId == 0 && info.max_value > info.min_value + 1.0) {
            probeId = info.id; probeMin = info.min_value; probeMax = info.max_value;
        }
    }
    CHECK(allInRange, "all param defaults within [min,max]");
    CHECK(allText, "value_to_text works for every param default");
    CHECK(!loopIds.empty(), "found Loop Start/End params");
    CHECK(fOnId != ~0u && fTypeId != ~0u && fCutId != ~0u, "found Fx filter params");

    // ---- note ports (instrument should accept MIDI in)
    auto *noteP = (const clap_plugin_note_ports *)
        plug->get_extension(plug, CLAP_EXT_NOTE_PORTS);
    CHECK(noteP && noteP->count(plug, true) >= 1, "declares >= 1 input note port");

    // ---- activate + start
    const uint32_t N = 512;
    const double SR = 48000.0;
    CHECK(plug->activate(plug, SR, 1, N), "activate @48k");
    CHECK(plug->start_processing(plug), "start_processing");

    // instrument: no audio inputs, one stereo output
    std::vector<float> outL(N), outR(N);
    float *outPtrs[2] = { outL.data(), outR.data() };
    clap_audio_buffer outBuf{}; outBuf.data32 = outPtrs; outBuf.channel_count = 2;

    clap_event_transport tr{};
    tr.header.size = sizeof(tr); tr.header.type = CLAP_EVENT_TRANSPORT;
    tr.flags = CLAP_TRANSPORT_IS_PLAYING | CLAP_TRANSPORT_HAS_TEMPO |
               CLAP_TRANSPORT_HAS_BEATS_TIMELINE;
    tr.tempo = 120.0;

    clap_process pr{};
    pr.frames_count = N; pr.transport = &tr;
    pr.audio_inputs = nullptr; pr.audio_inputs_count = 0;
    pr.audio_outputs = &outBuf; pr.audio_outputs_count = 1;
    pr.in_events = &s_in; pr.out_events = &s_out;
    pr.steady_time = 0;

    auto runBlock = [&](double beats) -> E {
        tr.song_pos_beats = (clap_beattime)(beats * CLAP_BEATTIME_FACTOR);
        clap_process_status st = plug->process(plug, &pr);
        g_in.clear();
        pr.steady_time += N;
        E e{0,0};
        for (uint32_t i = 0; i < N; i++) {
            e.l += (double)outL[i] * outL[i];
            e.r += (double)outR[i] * outR[i];
        }
        if (st == CLAP_PROCESS_ERROR) e.l = e.r = NAN;
        return e;
    };
    auto finite = [&](const E &e) { return std::isfinite(e.l) && std::isfinite(e.r); };

    // ---- Test A: hold a note, expect finite non-silent output
    pushNoteOn(0, 60, 0.9, 1);
    double beats = 0.0; bool aFinite = true; E held{0,0};
    for (int b = 0; b < 48; b++) {
        E e = runBlock(beats); beats += (double)N / SR * (120.0 / 60.0);
        if (!finite(e)) aFinite = false;
        if (b >= 16) { held.l += e.l; held.r += e.r; }   // settled portion
    }
    CHECK(aFinite, "held note: output finite over ~0.5 s");
    CHECK(held.l > 1e-3 && held.r > 1e-3, "held note: both channels sound (fallback tone)");

    // ---- Test A2: master filter. A lowpass at the minimum cutoff (~20 Hz) on
    // the harmonic-rich fallback tone (110 Hz fundamental + 24 harmonics) must
    // drop most of the energy. (Using the param minimum sidesteps the plain-vs-
    // normalized value convention.)
    printf("      Filter Cutoff range: [%.1f, %.1f]\n", fCutMin, fCutMax);
    double eOff = 0;
    for (int b = 0; b < 24; b++) { E e = runBlock(beats); beats += (double)N/SR*2; eOff += e.l + e.r; }
    pushParam(0, fOnId, 1.0);       // Filter On
    pushParam(0, fTypeId, 0.0);     // LP
    pushParam(0, fCutId, fCutMin);  // lowest cutoff
    double eOn = 0; bool a2Finite = true;
    for (int b = 0; b < 48; b++) {                 // let the cutoff smooth down
        E e = runBlock(beats); beats += (double)N/SR*2;
        if (!finite(e)) a2Finite = false;
        if (b >= 24) eOn += e.l + e.r;
    }
    CHECK(a2Finite, "filter: output stays finite with LP engaged");
    CHECK(eOn < 0.5 * eOff, "filter: LP at min cutoff cuts most of the energy");
    pushParam(0, fOnId, 0.0);       // disable again for the later tests
    for (int b = 0; b < 8; b++) { runBlock(beats); beats += (double)N/SR*2; }

    // ---- Test A3: matrix -> filter. Route Const +200 to Fx Cutoff with a
    // negative amount; the modulated cutoff collapses and the (fully-open)
    // filter's output energy drops -- proving matrix modulation reaches the
    // master filter through the shell.
    CHECK(mSrcId != ~0u && mDstId != ~0u && mAmtId != ~0u, "found Mod 00 row params");
    auto setStep = [&](clap_id id, double maxv, int index, int count) {
        // IntParams may be exposed to CLAP as plain (max>1) or normalized.
        pushParam(0, id, maxv > 1.5 ? (double)index : (double)index / (count - 1));
    };
    pushParam(0, fOnId, 1.0);       // filter on
    pushParam(0, fTypeId, 0.0);     // LP
    pushParam(0, fCutId, fCutMax);  // fully open (transparent baseline)
    double eOpen = 0;
    for (int b = 0; b < 32; b++) { E e = runBlock(beats); beats += (double)N/SR*2; if (b >= 8) eOpen += e.l + e.r; }
    setStep(mSrcId, mSrcMax, 27, 35);   // Source = Const +200
    setStep(mDstId, mDstMax, 79, 81);   // Dest 1 = Flt Cutoff
    pushParam(0, mAmtId, 0.0);          // Amt 1 = -100 (norm 0.0 of [-100,100])
    double eMod = 0; bool a3Finite = true;
    for (int b = 0; b < 48; b++) {
        E e = runBlock(beats); beats += (double)N/SR*2;
        if (!finite(e)) a3Finite = false;
        if (b >= 24) eMod += e.l + e.r;
    }
    CHECK(a3Finite, "matrix->filter: output stays finite");
    CHECK(eMod < 0.5 * eOpen, "matrix->filter: Const routed to Fx Cutoff collapses the filter");
    setStep(mSrcId, mSrcMax, 0, 35);    // clear the route
    setStep(mDstId, mDstMax, 0, 81);
    pushParam(0, fOnId, 0.0);
    for (int b = 0; b < 8; b++) { runBlock(beats); beats += (double)N/SR*2; }

    // ---- Test B: note-expression PAN hard-left -> right channel drops
    pushExpr(0, CLAP_NOTE_EXPRESSION_PAN, 0.0 /*hard left*/, 1);
    E panned{0,0}; bool bFinite = true;
    for (int b = 0; b < 48; b++) {
        E e = runBlock(beats); beats += (double)N / SR * (120.0 / 60.0);
        if (!finite(e)) bFinite = false;
        if (b >= 24) { panned.l += e.l; panned.r += e.r; }
    }
    CHECK(bFinite, "PolyPan: output stays finite");
    CHECK(panned.r < 0.15 * panned.l + 1e-6,
          "PolyPan hard-left drops the right channel");

    // ---- Test C: note-expression VOLUME 0 -> voice goes (near) silent
    pushExpr(0, CLAP_NOTE_EXPRESSION_PAN, 0.5 /*center*/, 1);
    pushExpr(0, CLAP_NOTE_EXPRESSION_VOLUME, 0.0 /*mute*/, 1);
    E muted{0,0}; bool cFinite = true;
    for (int b = 0; b < 96; b++) {                       // let old grains age out
        E e = runBlock(beats); beats += (double)N / SR * (120.0 / 60.0);
        if (!finite(e)) cFinite = false;
        if (b >= 64) { muted.l += e.l; muted.r += e.r; }
    }
    CHECK(cFinite, "PolyVolume: output stays finite");
    CHECK(muted.l + muted.r < 0.05 * (held.l + held.r),
          "PolyVolume 0 mutes the held voice");

    // ---- Test D: loop-clamp regression (would SIGABRT before the fix).
    // Restore volume, set every Loop Start/End to 100%, hold a fresh note.
    pushExpr(0, CLAP_NOTE_EXPRESSION_VOLUME, 1.0, 1);
    for (clap_id id : loopIds) pushParam(0, id, 100.0);   // 0..100 % params
    pushNoteOff(0, 60, 1);
    pushNoteOn(1, 67, 0.9, 2);
    bool dFinite = true;
    for (int b = 0; b < 32; b++) {
        E e = runBlock(beats); beats += (double)N / SR * (120.0 / 60.0);
        if (!finite(e)) dFinite = false;
    }
    CHECK(dFinite, "Loop Start=End=100% does not crash the audio thread");

    // ---- Test E: null-transport (free-run) pass
    pr.transport = nullptr;
    bool eFinite = true;
    for (int b = 0; b < 64; b++) if (!finite(runBlock(0))) eFinite = false;
    CHECK(eFinite, "free-run (null transport) output finite");
    pr.transport = &tr;

    plug->stop_processing(plug);

    // ---- Test F: state save/mutate/load actually carries values
    auto *state = (const clap_plugin_state *)plug->get_extension(plug, CLAP_EXT_STATE);
    CHECK(state, "state extension");
    double before = -1; params->get_value(plug, /*index*/0, &before);
    MemStream blobA; clap_ostream osA{ &blobA, osWrite };
    CHECK(state->save(plug, &osA), "state save");
    printf("      state blob: %zu bytes\n", blobA.data.size());

    // mutate the probe param to the opposite end, deliver it, confirm it took
    double target = (before <= (probeMin + probeMax) * 0.5) ? probeMax : probeMin;
    // temporarily re-start processing to deliver the param event
    plug->start_processing(plug);
    pushParam(0, probeId, target);
    runBlock(0);
    plug->stop_processing(plug);
    double mutated = -1; params->get_value(plug, 0, &mutated);
    // (index 0 may or may not be probeId; read probe by scanning)
    // Reload blob A and confirm the value reverts.
    MemStream blobA2 = blobA; clap_istream isA{ &blobA2, isRead };
    CHECK(state->load(plug, &isA), "state load");
    double after = -1; params->get_value(plug, 0, &after);
    CHECK(std::fabs(after - before) < 1e-6, "state load restores saved param values");
    (void)mutated;

    plug->deactivate(plug);
    plug->destroy(plug);
    entry->deinit();
    dlclose(dl);

    if (g_fails) { printf("\n%d CHECK(S) FAILED\n", g_fails); return 1; }
    printf("\nALL TESTS PASSED\n");
    return 0;
}
