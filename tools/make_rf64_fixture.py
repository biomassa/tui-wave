#!/usr/bin/env python3
"""Generate a large RF64 multichannel WAV shaped like the Max 9 files tui-wave has to open.

Mirrors ../temp-wavs/all215941.wav: 48kHz 32-bit float, a 52-byte JUNK placeholder at offset 12
that becomes `ds64` once the file passes 4GB, and a channel population that is mostly empty with a
handful carrying real signal — which is what makes Remove Empty Channels worth running.

The signal varies on a 20-second cycle so the zoomed-out overview has visible structure (rather
than a featureless rectangle) and scrolling shows something different as you go.
"""
import argparse, os, struct, sys
import numpy as np

SR = 48000
JUNK_BODY = 52          # whole chunk = 60 bytes = a ds64 with two table entries
DS64_BODY = 28
SENTINEL = 0xFFFFFFFF
CHUNK_SECS = 1          # frames generated per write
CYCLE_SECS = 20         # period of the slow amplitude/structure variation


def channel_plan(channels):
    """(kind, amplitude, freq) per channel — deliberately uneven, like a real capture.

    Peaks are chosen so Remove Empty Channels at its -48 dBFS default has a clear answer:
    'silent', 'denormal' and 'faint' are all below it; everything else is above.
    """
    plan = []
    for c in range(channels):
        if c in (0, 1):
            plan.append(('tone', 0.85, 110.0 * (1 + 0.5 * c)))      # loud pair, ~-1.4 dBFS
        elif c in (2, 3):
            plan.append(('tone', 0.35, 220.0 + 30 * c))             # mid pair, ~-9 dBFS
        elif c in (10, 11):
            plan.append(('sweep', 0.6, 0.0))                        # moving content, ~-4 dBFS
        elif c in (20, 21):
            plan.append(('tone', 0.02, 440.0))                      # quiet but real, -34 dBFS
        elif c in (30, 31):
            plan.append(('denormal', 3.8e-36, 0.0))                 # the real file has these
        elif c in (40, 41):
            plan.append(('faint', 0.0005, 880.0))                   # -66 dBFS, below -48
        else:
            plan.append(('silent', 0.0, 0.0))
    return plan


def build_second(plan, channels, second):
    """One second of interleaved float32 for every channel, as a flat array."""
    n = SR
    t = (second + np.arange(n, dtype=np.float64) / n)  # absolute seconds
    # Slow envelope on a CYCLE_SECS period: a raised cosine, so the overview shows a clear
    # repeating swell and a reader can tell one part of the timeline from another.
    env = 0.25 + 0.75 * (0.5 - 0.5 * np.cos(2 * np.pi * (second % CYCLE_SECS) / CYCLE_SECS))

    out = np.zeros((n, channels), dtype=np.float32)
    for c, (kind, amp, freq) in enumerate(plan):
        if kind == 'silent':
            continue
        if kind == 'denormal':
            out[:, c] = np.float32(amp)
            continue
        if kind == 'sweep':
            # 200Hz +/- 150Hz, modulated on the same 20s cycle.
            inst = 200 + 150 * np.sin(2 * np.pi * t / CYCLE_SECS)
            phase = 2 * np.pi * np.cumsum(inst) / SR
            out[:, c] = (amp * env * np.sin(phase)).astype(np.float32)
            continue
        out[:, c] = (amp * env * np.sin(2 * np.pi * freq * t)).astype(np.float32)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('path')
    ap.add_argument('--channels', type=int, default=56)
    ap.add_argument('--gb', type=float, default=30.0, help='target size in GiB')
    args = ap.parse_args()

    ch = args.channels
    bpf = ch * 4
    frames = int(args.gb * (1 << 30)) // bpf
    # Whole seconds, so the cycle lands cleanly and the last block is not a stub.
    frames -= frames % SR
    data_bytes = frames * bpf
    plan = channel_plan(ch)
    will_be_rf64 = data_bytes > 0xFFFFFFFF

    kinds = {}
    for kind, _, _ in plan:
        kinds[kind] = kinds.get(kind, 0) + 1
    print(f"channels={ch} rate={SR} float32 bytes/frame={bpf}")
    print(f"frames={frames}  duration={frames/SR/60:.1f} min  data={data_bytes/(1<<30):.2f} GiB")
    print(f"4GB crossover at {(1<<32)/(SR*bpf)/60:.1f} min -> RF64: {will_be_rf64}")
    print(f"channel kinds: {kinds}")

    with open(args.path, 'wb') as f:
        f.write(b'RIFF')
        f.write(struct.pack('<I', 0))          # patched at the end
        f.write(b'WAVE')
        f.write(b'JUNK')
        f.write(struct.pack('<I', JUNK_BODY))
        f.write(b'\0' * JUNK_BODY)
        f.write(b'fmt ')
        f.write(struct.pack('<I', 16))
        f.write(struct.pack('<HHIIHH', 3, ch, SR, SR * bpf, bpf, 32))
        f.write(b'data')
        data_size_at = f.tell()
        f.write(struct.pack('<I', 0))          # patched at the end
        data_start = f.tell()

        # The signal repeats every CYCLE_SECS, so only that many seconds need generating.
        cache = [build_second(plan, ch, s).tobytes() for s in range(CYCLE_SECS)]
        total_secs = frames // SR
        next_report = 1 << 30
        for s in range(total_secs):
            f.write(cache[s % CYCLE_SECS])
            done = (s + 1) * SR * bpf
            if done >= next_report:
                sys.stdout.write(f"\r  {done/(1<<30):6.2f} / {data_bytes/(1<<30):.2f} GiB"
                                 f"  {100.0*done/data_bytes:5.1f}%")
                sys.stdout.flush()
                next_report += 1 << 30
        print()

        total = f.tell()
        riff_size = total - 8

        if will_be_rf64 or riff_size > 0xFFFFFFFF:
            f.seek(0)
            f.write(b'RF64')
            f.write(struct.pack('<I', SENTINEL))
            f.seek(12)
            f.write(b'ds64')
            f.write(struct.pack('<I', DS64_BODY))
            f.write(struct.pack('<Q', riff_size))
            f.write(struct.pack('<Q', data_bytes))
            f.write(struct.pack('<Q', frames))
            f.write(struct.pack('<I', 0))                       # tableLength
            f.write(b'\0' * (JUNK_BODY - DS64_BODY))            # zero the unused reservation
            f.seek(data_size_at)
            f.write(struct.pack('<I', SENTINEL))
            form = 'RF64'
        else:
            f.seek(4)
            f.write(struct.pack('<I', riff_size))
            f.seek(data_size_at)
            f.write(struct.pack('<I', data_bytes))
            form = 'RIFF'

    size = os.path.getsize(args.path)
    print(f"wrote {args.path}")
    print(f"  {form}, {size/(1<<30):.2f} GiB on disk, data at offset {data_start}")


if __name__ == '__main__':
    main()
