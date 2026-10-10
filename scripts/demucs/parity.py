"""Write a 20 s synthetic mix and Demucs's own stems of it (`apply_model`, shifts=0,
overlap=0.25) for the `separation_matches_demucs_and_the_stems_sum_to_the_mix` test:

    .venv/bin/python scripts/demucs/parity.py <dir>   # then copy the export to <dir>/model.onnx
    KERF_DEMUCS_PARITY_DIR=<dir> cargo test -p kerf-core --no-default-features --release \
        separation_matches -- --ignored
"""
import sys, math, torch, numpy as np, soundfile as sf
from demucs.pretrained import get_model
from demucs.apply import apply_model
out = sys.argv[1]
sr = 44100; n = sr * 20
t = np.arange(n) / sr
rng = np.random.default_rng(1)
bpm = 100; beat = 60 / bpm
mix = np.zeros((2, n))
# bass line, chords, hats, kick, a "voice" (vibrato tone)
for k in range(int(20 / beat)):
    s = int(k * beat * sr); e = min(n, s + int(0.25 * sr)); tt = np.arange(e - s) / sr
    mix[:, s:e] += 0.6 * np.exp(-tt * 20) * np.sin(2 * np.pi * 55 * tt)          # kick
    h = int((k + 0.5) * beat * sr); he = min(n, h + 2000)
    if h < n: mix[:, h:he] += 0.05 * rng.standard_normal(he - h) * np.exp(-np.arange(he - h) / 300)
bass = 0.25 * np.sin(2 * np.pi * np.where((t // (4 * beat)) % 2 == 0, 82.4, 98.0) * t)
chords = sum(0.08 * np.sin(2 * np.pi * f * t) for f in (261.6, 329.6, 392.0))
voice = 0.15 * np.sin(2 * np.pi * (440 + 6 * np.sin(2 * np.pi * 5 * t)) * t) * (np.sin(2 * np.pi * t / 4) > 0)
mix[0] += bass + chords + voice; mix[1] += bass + 0.8 * chords + voice
mix = mix.astype(np.float32)
sf.write(f"{out}/mix.wav", mix.T, sr, subtype="FLOAT")
m = get_model("htdemucs")
wav = torch.from_numpy(mix)
ref = wav.mean(0)
w = (wav - ref.mean()) / ref.std()
with torch.no_grad():
    srcs = apply_model(m, w[None], shifts=0, split=True, overlap=0.25, progress=False)[0]
srcs = srcs * ref.std() + ref.mean()
for name, s in zip(m.sources, srcs):
    sf.write(f"{out}/ref_{name}.wav", s.numpy().T, sr, subtype="FLOAT")
rec = srcs.sum(0) - wav
print("python reconstruction rms", rec.pow(2).mean().sqrt().item(), "mix rms", wav.pow(2).mean().sqrt().item())
