"""A deterministic Matroska movie built without an encoder.

The video is H.264 Constrained Baseline in which every macroblock is I_PCM:
the samples are stored raw, so the bitstream is valid and decodable while
this module needs no codec. Every frame is an IDR picture. The file also
carries 16-bit PCM audio, a UTF-8 subtitle track and Cues, which is what
GStreamer's HLS module needs: a Matroska container, a supported video codec,
an audio track to transcode and a cue timeline (R7.8).
"""

from __future__ import annotations

import math
import struct

WIDTH, HEIGHT = 64, 48
FPS = 10
SECONDS = 8
CLUSTER_SECONDS = 2
AUDIO_RATE = 8000
SUBTITLES = ((1000, 3000, "Hello from Rustorr"), (4500, 6500, "Второй титр"))


class BitWriter:
    def __init__(self) -> None:
        self.data = bytearray()
        self.current = 0
        self.count = 0

    def u(self, width: int, value: int) -> None:
        for shift in range(width - 1, -1, -1):
            self.current = (self.current << 1) | ((value >> shift) & 1)
            self.count += 1
            if self.count == 8:
                self.data.append(self.current)
                self.current = 0
                self.count = 0

    def ue(self, value: int) -> None:
        value += 1
        width = value.bit_length()
        self.u(width - 1, 0)
        self.u(width, value)

    def se(self, value: int) -> None:
        self.ue(2 * value - 1 if value > 0 else -2 * value)

    def align_zero(self) -> None:
        while self.count:
            self.u(1, 0)

    def raw(self, data: bytes) -> None:
        assert self.count == 0
        self.data.extend(data)

    def trailing(self) -> bytes:
        self.u(1, 1)
        self.align_zero()
        return bytes(self.data)


def nal(nal_ref_idc: int, nal_type: int, rbsp: bytes) -> bytes:
    """Header plus the RBSP with emulation prevention bytes."""
    out = bytearray([(nal_ref_idc << 5) | nal_type])
    zeros = 0
    for byte in rbsp:
        if zeros >= 2 and byte <= 3:
            out.append(3)
            zeros = 0
        out.append(byte)
        zeros = zeros + 1 if byte == 0 else 0
    return bytes(out)


def sps() -> bytes:
    bits = BitWriter()
    bits.u(8, 66)  # Baseline
    bits.u(8, 0xC0)  # constraint_set0 and set1: Constrained Baseline
    bits.u(8, 10)  # level 1.0
    bits.ue(0)  # seq_parameter_set_id
    bits.ue(0)  # log2_max_frame_num_minus4
    bits.ue(2)  # pic_order_cnt_type: output order is decode order
    bits.ue(1)  # max_num_ref_frames
    bits.u(1, 0)  # gaps_in_frame_num_value_allowed_flag
    bits.ue(WIDTH // 16 - 1)
    bits.ue(HEIGHT // 16 - 1)
    bits.u(1, 1)  # frame_mbs_only_flag
    bits.u(1, 1)  # direct_8x8_inference_flag
    bits.u(1, 0)  # frame_cropping_flag
    bits.u(1, 0)  # vui_parameters_present_flag
    return nal(3, 7, bits.trailing())


def pps() -> bytes:
    bits = BitWriter()
    bits.ue(0)  # pic_parameter_set_id
    bits.ue(0)  # seq_parameter_set_id
    bits.u(1, 0)  # entropy_coding_mode_flag: CAVLC
    bits.u(1, 0)  # bottom_field_pic_order_in_frame_present_flag
    bits.ue(0)  # num_slice_groups_minus1
    bits.ue(0)  # num_ref_idx_l0_default_active_minus1
    bits.ue(0)  # num_ref_idx_l1_default_active_minus1
    bits.u(1, 0)  # weighted_pred_flag
    bits.u(2, 0)  # weighted_bipred_idc
    bits.se(0)  # pic_init_qp_minus26
    bits.se(0)  # pic_init_qs_minus26
    bits.se(0)  # chroma_qp_index_offset
    bits.u(1, 1)  # deblocking_filter_control_present_flag
    bits.u(1, 0)  # constrained_intra_pred_flag
    bits.u(1, 0)  # redundant_pic_cnt_present_flag
    return nal(3, 8, bits.trailing())


def idr_slice(frame: int) -> bytes:
    bits = BitWriter()
    bits.ue(0)  # first_mb_in_slice
    bits.ue(7)  # slice_type: I, all slices of the picture
    bits.ue(0)  # pic_parameter_set_id
    bits.u(4, 0)  # frame_num
    bits.ue(frame % 2)  # idr_pic_id differs between consecutive IDR pictures
    bits.u(1, 0)  # no_output_of_prior_pics_flag
    bits.u(1, 0)  # long_term_reference_flag
    bits.se(0)  # slice_qp_delta
    bits.ue(1)  # disable_deblocking_filter_idc
    for mb_y in range(HEIGHT // 16):
        for mb_x in range(WIDTH // 16):
            bits.ue(25)  # mb_type I_PCM
            bits.align_zero()
            luma = bytes(
                16 + ((2 * (mb_x * 16 + x) + (mb_y * 16 + y) + 6 * frame) % 200)
                for y in range(16)
                for x in range(16)
            )
            cb = bytes([128 + (frame * 5) % 64] * 64)
            cr = bytes([160 - (mb_x * 16 + mb_y * 8) % 64] * 64)
            bits.raw(luma + cb + cr)
    return nal(3, 5, bits.trailing())


def avcc(sps_nal: bytes, pps_nal: bytes) -> bytes:
    return (
        bytes([1, sps_nal[1], sps_nal[2], sps_nal[3], 0xFF, 0xE1])
        + struct.pack(">H", len(sps_nal))
        + sps_nal
        + bytes([1])
        + struct.pack(">H", len(pps_nal))
        + pps_nal
    )


def vint(size: int) -> bytes:
    length = 1
    while size >= (1 << (7 * length)) - 1:
        length += 1
    return (size | (1 << (7 * length))).to_bytes(length, "big")


def element(element_id: int, payload: bytes) -> bytes:
    return element_id.to_bytes((element_id.bit_length() + 7) // 8, "big") + vint(len(payload)) + payload


def uint(element_id: int, value: int, width: int = 0) -> bytes:
    width = width or max(1, (value.bit_length() + 7) // 8)
    return element(element_id, value.to_bytes(width, "big"))


def double(element_id: int, value: float) -> bytes:
    return element(element_id, struct.pack(">d", value))


def text(element_id: int, value: str) -> bytes:
    return element(element_id, value.encode())


def block_header(track: int, relative_ms: int, flags: int) -> bytes:
    return vint(track) + struct.pack(">hB", relative_ms, flags)


def audio_block(start_sample: int, count: int) -> bytes:
    return b"".join(
        struct.pack("<h", int(12000 * math.sin(2 * math.pi * 440 * index / AUDIO_RATE)))
        for index in range(start_sample, start_sample + count)
    )


def matroska() -> bytes:
    sps_nal, pps_nal = sps(), pps()
    ebml = element(
        0x1A45DFA3,
        uint(0x4286, 1)
        + uint(0x42F7, 1)
        + uint(0x42F2, 4)
        + uint(0x42F3, 8)
        + text(0x4282, "matroska")
        + uint(0x4287, 4)
        + uint(0x4285, 2),
    )
    info = element(
        0x1549A966,
        uint(0x2AD7B1, 1_000_000)
        + double(0x4489, SECONDS * 1000.0)
        + text(0x4D80, "rustorr fixtures")
        + text(0x5741, "rustorr fixtures"),
    )
    tracks = element(
        0x1654AE6B,
        element(
            0xAE,
            uint(0xD7, 1)
            + uint(0x73C5, 1)
            + uint(0x83, 1)
            + uint(0x9C, 0)
            + text(0x22B59C, "und")
            + text(0x86, "V_MPEG4/ISO/AVC")
            + element(0x63A2, avcc(sps_nal, pps_nal))
            + uint(0x23E383, 1_000_000_000 // FPS)
            + element(0xE0, uint(0xB0, WIDTH) + uint(0xBA, HEIGHT)),
        )
        + element(
            0xAE,
            uint(0xD7, 2)
            + uint(0x73C5, 2)
            + uint(0x83, 2)
            + uint(0x9C, 0)
            + text(0x22B59C, "rus")
            + text(0x86, "A_PCM/INT/LIT")
            + element(0xE1, double(0xB5, float(AUDIO_RATE)) + uint(0x9F, 1) + uint(0x6264, 16)),
        )
        + element(
            0xAE,
            uint(0xD7, 3)
            + uint(0x73C5, 3)
            + uint(0x83, 0x11)
            + uint(0x9C, 0)
            + text(0x536E, "English")
            + text(0x22B59C, "eng")
            + text(0x86, "S_TEXT/UTF8"),
        ),
    )

    frame_ms = 1000 // FPS
    samples_per_frame = AUDIO_RATE // FPS
    clusters = []
    for cluster_index in range(SECONDS // CLUSTER_SECONDS):
        cluster_ms = cluster_index * CLUSTER_SECONDS * 1000
        body = uint(0xE7, cluster_ms)
        for local in range(CLUSTER_SECONDS * FPS):
            frame = cluster_index * CLUSTER_SECONDS * FPS + local
            relative = local * frame_ms
            slice_nal = idr_slice(frame)
            body += element(0xA3, block_header(1, relative, 0x80) + struct.pack(">I", len(slice_nal)) + slice_nal)
            body += element(0xA3, block_header(2, relative, 0x80) + audio_block(frame * samples_per_frame, samples_per_frame))
            for start, end, line in SUBTITLES:
                if start == cluster_ms + relative:
                    body += element(
                        0xA0,
                        element(0xA1, block_header(3, relative, 0) + line.encode()) + uint(0x9B, end - start),
                    )
        clusters.append(element(0x1F43B675, body))

    def seek_head(info_at: int, tracks_at: int, cues_at: int) -> bytes:
        def seek(target: int, position: int) -> bytes:
            return element(0x4DBB, uint(0x53AB, target, 4) + uint(0x53AC, position, 4))

        return element(0x114D9B74, seek(0x1549A966, info_at) + seek(0x1654AE6B, tracks_at) + seek(0x1C53BB6B, cues_at))

    head_size = len(seek_head(0, 0, 0))
    info_at = head_size
    tracks_at = info_at + len(info)
    cluster_at = []
    position = tracks_at + len(tracks)
    for cluster in clusters:
        cluster_at.append(position)
        position += len(cluster)
    cues = element(
        0x1C53BB6B,
        b"".join(
            element(
                0xBB,
                uint(0xB3, index * CLUSTER_SECONDS * 1000)
                + element(0xB7, uint(0xF7, 1) + uint(0xF1, at)),
            )
            for index, at in enumerate(cluster_at)
        ),
    )
    segment = seek_head(info_at, tracks_at, position) + info + tracks + b"".join(clusters) + cues
    return ebml + element(0x18538067, segment)
