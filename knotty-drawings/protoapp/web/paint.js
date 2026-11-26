"use strict";

// One request in flight, with only the latest unsent pointer position retained.
// Finish waits for that request; Cancel drops unsent picks, OK flushes them.
class PaintSession {
  // `begin` adds fields to the opening request, such as a drawing control's index.
  constructor({ request, session, snapshot, key, field, drawing, status, profile, beginAction = "begin", begin = {} }) {
    Object.assign(this, { request, session, snapshot, drawing, status, profile });
    this.pending = null;
    this.running = null;
    this.finishing = false;
    this.failed = false;
    this.ready = request({ action: beginAction, session, snapshot, key, field, ...begin }).then(result => {
      if (!result.ok) throw new Error(result.reason);
      this.snapshot = result.drawing.snapshot;
      this.began = true;
      return true;
    }).catch(error => { this.failed = true; status(error.message, true); return false; });
  }
  pick(value) {
    if (this.finishing) return;
    this.pending = { value };
    this.pump();
  }
  pump() {
    if (this.running) return this.running;
    this.running = this.drain().finally(() => { this.running = null; });
    return this.running;
  }
  async drain() {
    if (!await this.ready) { this.pending = null; return; }
    while (this.pending) {
      const { value } = this.pending;
      this.pending = null;
      const start = performance.now();
      this.status("Previewing change…", false);
      try {
        const result = await this.request({ action: "update", session: this.session, snapshot: this.snapshot, value });
        if (!result.ok) throw new Error(result.reason);
        const responseMs = performance.now() - start;
        this.snapshot = result.drawing.snapshot;
        const domStart = performance.now();
        await this.drawing(result.drawing);
        this.profile({ ...result.timings, response_ms: responseMs, dom_ms: performance.now() - domStart });
        this.failed = false;
        this.status("Preview applied. OK keeps it; Cancel restores the original.", false);
      } catch (error) {
        this.failed = true;
        this.status(error.message, true);
      }
    }
  }
  async finish(keep) {
    if (this.finishing) return false;
    this.finishing = true;
    if (!keep) this.pending = null;
    await this.ready;
    if (this.running) await this.running;
    if (keep && this.failed) { this.finishing = false; return false; }
    try {
      const result = await this.request({ action: keep ? "accept" : "cancel", session: this.session });
      if (result.drawing) await this.drawing(result.drawing);
      if (!result.ok) {
        this.finishError = result.reason;
        this.status(result.reason, true);
        // A begin failure has no owned transaction to retain. An external edit
        // closes a stale transaction without overwriting the external source.
        if (!result.closed && this.began) { this.finishing = false; return false; }
      }
      return true;
    } catch (error) {
      this.status(error.message, true);
      this.finishing = false;
      return false;
    }
  }
}

function hsvToRgb(h, s, v) {
  const c = v * s, x = c * (1 - Math.abs((h / 60) % 2 - 1)), m = v - c;
  const channels = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return channels.map(n => Math.round((n + m) * 255));
}
function rgbToHsv(rgb) {
  const [r, g, b] = rgb.map(c => c / 255), max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
  let h = 0;
  if (d) h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, max ? d / max : 0, max];
}
if (typeof module !== "undefined") module.exports = { PaintSession, hsvToRgb, rgbToHsv };
