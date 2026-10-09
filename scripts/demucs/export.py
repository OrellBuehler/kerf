"""Export the network of Demucs `htdemucs` to ONNX, without its STFT.

The model's own STFT/iSTFT do not export, so Kerf computes them (engine/stems.rs) and
the ONNX takes `mix` [1, 2, 343980] and the complex-as-channels spectrogram `mag`
[1, 4, 2048, 336] and returns `spec_out` [1, 4, 4, 2048, 336] and `wave_out`
[1, 4, 2, 343980]. Checks the export against the PyTorch model before writing.

    uv venv .venv && uv pip install -p .venv/bin/python --index-url \
        https://download.pytorch.org/whl/cpu torch==2.5.1 torchaudio==2.5.1
    uv pip install -p .venv/bin/python demucs==4.0.1 onnx==1.17.0 onnxruntime==1.20.1 numpy==1.26.4 soundfile
    .venv/bin/python scripts/demucs/export.py <out-dir>
"""
import sys, math, torch, numpy as np
from demucs.pretrained import get_model
from demucs.htdemucs import HTDemucs
from einops import rearrange

out_dir = sys.argv[1]
torch.manual_seed(0)
bag = get_model("htdemucs")
m: HTDemucs = bag.models[0]
m.eval()
L = int(m.segment * m.samplerate)
print("sources", m.sources, "sr", m.samplerate, "segment", float(m.segment), "L", L, "nfft", m.nfft, "hop", m.hop_length, "cac", m.cac)

class Core(torch.nn.Module):
    def __init__(self, m):
        super().__init__()
        self.m = m
    def forward(self, mix, mag):
        self_ = self.m
        x = mag
        B, C, Fq, T = x.shape
        mean = x.mean(dim=(1, 2, 3), keepdim=True)
        std = x.std(dim=(1, 2, 3), keepdim=True)
        x = (x - mean) / (1e-5 + std)
        xt = mix
        meant = xt.mean(dim=(1, 2), keepdim=True)
        stdt = xt.std(dim=(1, 2), keepdim=True)
        xt = (xt - meant) / (1e-5 + stdt)
        saved, saved_t, lengths, lengths_t = [], [], [], []
        for idx, encode in enumerate(self_.encoder):
            lengths.append(x.shape[-1])
            inject = None
            if idx < len(self_.tencoder):
                lengths_t.append(xt.shape[-1])
                tenc = self_.tencoder[idx]
                xt = tenc(xt)
                if not tenc.empty:
                    saved_t.append(xt)
                else:
                    inject = xt
            x = encode(x, inject)
            if idx == 0 and self_.freq_emb is not None:
                frs = torch.arange(x.shape[-2], device=x.device)
                emb = self_.freq_emb(frs).t()[None, :, :, None].expand_as(x)
                x = x + self_.freq_emb_scale * emb
            saved.append(x)
        if self_.crosstransformer:
            if self_.bottom_channels:
                b, c, f, t = x.shape
                x = rearrange(x, "b c f t-> b c (f t)")
                x = self_.channel_upsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = self_.channel_upsampler_t(xt)
            x, xt = self_.crosstransformer(x, xt)
            if self_.bottom_channels:
                x = rearrange(x, "b c f t-> b c (f t)")
                x = self_.channel_downsampler(x)
                x = rearrange(x, "b c (f t)-> b c f t", f=f)
                xt = self_.channel_downsampler_t(xt)
        for idx, decode in enumerate(self_.decoder):
            skip = saved.pop(-1)
            x, pre = decode(x, skip, lengths.pop(-1))
            offset = self_.depth - len(self_.tdecoder)
            if idx >= offset:
                tdec = self_.tdecoder[idx - offset]
                length_t = lengths_t.pop(-1)
                if tdec.empty:
                    pre = pre[:, :, 0]
                    xt, _ = tdec(pre, None, length_t)
                else:
                    skip = saved_t.pop(-1)
                    xt, _ = tdec(xt, skip, length_t)
        S = len(self_.sources)
        x = x.view(B, S, -1, Fq, T)
        x = x * std[:, None] + mean[:, None]
        xt = xt.view(B, S, -1, mix.shape[-1])
        xt = xt * stdt[:, None] + meant[:, None]
        return x, xt

core = Core(m).eval()
mix = torch.randn(1, 2, L) * 0.1
with torch.no_grad():
    z = m._spec(mix)
    mag = m._magnitude(z)
    print("z", tuple(z.shape), "mag", tuple(mag.shape))
    ref = m(mix)
    xs, xt = core(mix, mag)
    full = xt + m._ispec(m._mask(z, xs), L)
    print("wrapper vs model max diff", (full - ref).abs().max().item())
torch.onnx.export(core, (mix, mag), f"{out_dir}/htdemucs.onnx", input_names=["mix", "mag"], output_names=["spec_out", "wave_out"], opset_version=17, do_constant_folding=True)
import onnxruntime as ort
sess = ort.InferenceSession(f"{out_dir}/htdemucs.onnx", providers=["CPUExecutionProvider"])
o_spec, o_wave = sess.run(None, {"mix": mix.numpy(), "mag": mag.numpy()})
full_onnx = torch.from_numpy(o_wave) + m._ispec(m._mask(z, torch.from_numpy(o_spec)), L)
print("onnx vs model max diff", (full_onnx - ref).abs().max().item(), "rms", ((full_onnx - ref)**2).mean().sqrt().item())
