"""ctypes binding to the JIS Korean engine (jis-korean engine/include/jis_korean.h).

Only the read-only parts are used: typing keys into a private engine to read
its readings (``jk_predict``), its English reading, and its hanja candidates.
Nothing is ever committed through the engine here, recording is off, and the
engine handle is private to this process, so using it teaches the IME nothing
and stores nothing (IME contract: privacy).

Finding the engine (first hit wins):
  1. ``$JIS_KOREAN_ENGINE`` (path to libjis_korean.dylib)
  2. ``providers.jiskorean.engine`` in complete.json
  3. ``~/Projects/jis-korean/engine/target/release/libjis_korean.dylib``
     (``cd ~/Projects/jis-korean/engine && cargo build --release``)

Resources (``predict/`` and ``hanja.tsv``): ``$JIS_KOREAN_RESOURCES``,
``providers.jiskorean.resources``, the installed
``~/Library/Input Methods/JISKorean.app/Contents/Resources``, then the
source tree (``engine/predict``, ``engine/dict/hanja.tsv``).
"""

from __future__ import annotations

import ctypes as C
import os
import plistlib
import subprocess
import threading
from dataclasses import dataclass

BUNDLE_ID = "me.sjun.inputmethod.JISKorean"
SOURCE_ROOT = os.path.expanduser("~/Projects/jis-korean")
DEFAULT_DYLIB = os.path.join(SOURCE_ROOT, "engine/target/release/libjis_korean.dylib")
APP_RESOURCES = [os.path.expanduser("~/Library/Input Methods/JISKorean.app/Contents/Resources"),
                 "/Library/Input Methods/JISKorean.app/Contents/Resources"]

# Prediction kinds (JK_PREDICTION_*).
KOREAN, ENGLISH, ENGLISH_CAP, ENGLISH_UPPER, TWO_SET, KEEP_FINALS, HANJA, BARE_VOWELS, MIXED, \
    IMPLIED_L = range(10)
KIND_NAMES = {KOREAN: "korean", ENGLISH: "english", ENGLISH_CAP: "english", ENGLISH_UPPER: "english",
              TWO_SET: "korean-alt", KEEP_FINALS: "korean-alt", HANJA: "hanja",
              BARE_VOWELS: "korean-alt", MIXED: "mixed", IMPLIED_L: "korean-alt"}
WEIGHT_NAMES = ["Korean", "English", "Implied", "Mixed", "TwoSet", "KeepFinals", "Hanja", "ImpliedL"]

# Input source suffix -> engine mode id (jis-korean mac/Sources/InputModes.swift) and the
# Korean mode whose keyboard it sits on (English modes can't predict).
MODES = {"2Set": 0, "2SetANSI": 1, "2SetISO": 2, "A_QWERTY": 3, "B_QWERTY": 9, "CJ_QWERTY": 10,
         "A_DVORAK": 7, "A_COLEMAK": 8}
KOREAN_MODE_FOR = {0: 0, 1: 1, 2: 2, 3: 0, 7: 0, 8: 0, 9: 1, 10: 2}

# Letter layouts of the English modes, by row (Q row, A row, Z row) -> QWERTY positions.
_QWERTY = ("qwertyuiop", "asdfghjkl", "zxcvbnm")
_LAYOUTS = {
    "A_DVORAK": ("',.pyfgcrl", "aoeuidhtns", ";qjkxbm"),
    "A_COLEMAK": ("qwfpgjluy;", "arstdhneio", "zxcvbkm"),
}


class EngineUnavailable(Exception):
    pass


@dataclass
class Prediction:
    text: str
    kind: int

    @property
    def kind_name(self) -> str:
        return KIND_NAMES.get(self.kind, "other")


@dataclass
class Reading:
    preedit: str            # the deterministic JIS Korean reading of the keys
    english: str            # the keys read as English on the stock keyboard
    predictions: list[Prediction]


def find_dylib(configured: str | None = None) -> str | None:
    for cand in (os.environ.get("JIS_KOREAN_ENGINE"), configured, DEFAULT_DYLIB):
        if cand:
            cand = os.path.expanduser(cand)
            if os.path.isfile(cand):
                return cand
    return None


def find_resources(configured: str | None = None) -> tuple[str | None, str | None]:
    """(predict dir, hanja.tsv path), each None if not found."""
    roots = [os.environ.get("JIS_KOREAN_RESOURCES"), configured, *APP_RESOURCES]
    predict = hanja = None
    for r in roots:
        if not r:
            continue
        r = os.path.expanduser(r)
        if predict is None and os.path.isfile(os.path.join(r, "predict", "ranker.bin")):
            predict = os.path.join(r, "predict")
        if hanja is None and os.path.isfile(os.path.join(r, "hanja.tsv")):
            hanja = os.path.join(r, "hanja.tsv")
    if predict is None and os.path.isfile(os.path.join(SOURCE_ROOT, "engine/predict/ranker.bin")):
        predict = os.path.join(SOURCE_ROOT, "engine/predict")
    if hanja is None and os.path.isfile(os.path.join(SOURCE_ROOT, "engine/dict/hanja.tsv")):
        hanja = os.path.join(SOURCE_ROOT, "engine/dict/hanja.tsv")
    return predict, hanja


def read_ime_defaults(timeout: float = 1.0) -> dict:
    """The IME's preferences (weights, what it learned, training words), read
    once with ``defaults export``. Empty on any failure."""
    try:
        out = subprocess.run(["/usr/bin/defaults", "export", BUNDLE_ID, "-"], capture_output=True,
                             timeout=timeout, check=False).stdout
        return plistlib.loads(out) if out else {}
    except Exception:
        return {}


_P = C.c_void_p
_SZ = C.c_size_t
_U32 = C.c_uint32
_BUF = C.c_char_p

_SIGS = {
    "jk_new": (_P, []), "jk_free": (None, [_P]), "jk_clear": (None, [_P]),
    "jk_key": (_U32, [_P, _U32, _U32]), "jk_set_mode": (_U32, [_P, _U32]),
    "jk_preedit_ptr": (_P, [_P]), "jk_preedit_len": (_SZ, [_P]),
    "jk_english_ptr": (_P, [_P]), "jk_english_len": (_SZ, [_P]),
    "jk_key_count": (_U32, []), "jk_key_code_ptr": (_P, [_U32]), "jk_key_code_len": (_SZ, [_U32]),
    "jk_candidate_count": (_SZ, [_P]), "jk_candidate_ptr": (_P, [_P, _SZ]),
    "jk_candidate_len": (_SZ, [_P, _SZ]), "jk_candidate_comment_ptr": (_P, [_P, _SZ]),
    "jk_candidate_comment_len": (_SZ, [_P, _SZ]),
    "jk_dict_new": (_P, [_BUF, _SZ]), "jk_dict_free": (None, [_P]), "jk_set_dictionary": (None, [_P, _P]),
}
_PREDICT_SIGS = {
    "jk_predictor_new": (_P, [_BUF, _SZ, _BUF, _SZ, _BUF, _SZ, _BUF, _SZ]),
    "jk_predictor_free": (None, [_P]), "jk_set_predictor": (None, [_P, _P]),
    "jk_predict": (_SZ, [_P]), "jk_prediction_ptr": (_P, [_P, _SZ]),
    "jk_prediction_len": (_SZ, [_P, _SZ]), "jk_prediction_kind": (_U32, [_P, _SZ]),
}
_OPTIONAL_SIGS = {
    "jk_predictor_with_learned": (_P, [_P, _BUF, _SZ]),
    "jk_predictor_with_words": (_P, [_P, _BUF, _SZ, _BUF, _SZ]),
    "jk_predictor_with_avoided": (_P, [_P, _BUF, _SZ, _BUF, _SZ]),
    "jk_set_prediction_weight": (None, [_P, _U32, C.c_double]),
    "jk_set_learning": (None, [_P, _U32]), "jk_set_recording": (None, [_P, _U32]),
    "jk_learner_import": (_U32, [_P, _BUF, _SZ]),
}


class JisKoreanEngine:
    """One private engine handle. Calls are serialized with a lock."""

    def __init__(self, dylib: str, predict_dir: str | None, hanja_path: str | None,
                 ime_settings: dict | None = None) -> None:
        try:
            self.lib = C.CDLL(dylib)
        except OSError as exc:
            raise EngineUnavailable(f"cannot load {dylib}: {exc}") from exc
        self.path = dylib
        self.has = set()
        for table, required in ((_SIGS, True), (_PREDICT_SIGS, False), (_OPTIONAL_SIGS, False)):
            for name, (res, args) in table.items():
                try:
                    fn = getattr(self.lib, name)
                except AttributeError:
                    if required:
                        raise EngineUnavailable(f"{dylib} lacks {name}") from None
                    continue
                fn.restype, fn.argtypes = res, args
                self.has.add(name)
        self.can_predict = all(n in self.has for n in _PREDICT_SIGS)
        self._lock = threading.Lock()
        self._keep: list[bytes] = []  # buffers the engine may reference
        self.codes = {self._str(self.lib.jk_key_code_ptr(i), self.lib.jk_key_code_len(i)): i
                      for i in range(self.lib.jk_key_count())}
        self.handle = self.lib.jk_new()
        if not self.handle:
            raise EngineUnavailable("jk_new failed")
        self.dict = None
        self.predictor = None
        self._predictors: list[int] = []
        self.mode = 0
        self.notes: list[str] = []
        settings = ime_settings or {}
        if hanja_path:
            try:
                data = open(hanja_path, "rb").read()
                self._keep.append(data)
                self.dict = self.lib.jk_dict_new(data, len(data))
                if self.dict:
                    self.lib.jk_set_dictionary(self.handle, self.dict)
            except OSError:
                self.notes.append("hanja dictionary unreadable")
        else:
            self.notes.append("no hanja dictionary")
        if self.can_predict and predict_dir:
            self._load_predictor(predict_dir, settings)
        elif not self.can_predict:
            self.notes.append("lite engine build (no predictions)")
        else:
            self.notes.append("no predict/ resources")
        self._apply_settings(settings)

    # ------------------------------------------------------------- setup
    @staticmethod
    def _str(ptr, n) -> str:
        return C.string_at(ptr, n).decode("utf-8", "replace") if ptr and n else ""

    def _read(self, d: str, name: str) -> bytes | None:
        try:
            with open(os.path.join(d, name), "rb") as fh:
                return fh.read()
        except OSError:
            return None

    def _load_predictor(self, d: str, settings: dict) -> None:
        model, en, ko = self._read(d, "ranker.bin"), self._read(d, "common-en.txt"), self._read(d, "common-ko.txt")
        nouns = self._read(d, "english-nouns.txt") or b""
        if not (model and en and ko):
            self.notes.append("predict/ incomplete")
            return
        self._keep += [model, en, ko, nouns]
        base = self.lib.jk_predictor_new(model, len(model), en, len(en), ko, len(ko), nouns, len(nouns))
        if not base:
            self.notes.append("predictor model rejected")
            return
        self._predictors.append(base)
        pred = base
        learned = self._read(d, "learned.bin")
        if learned and "jk_predictor_with_learned" in self.has:
            self._keep.append(learned)
            ranked = self.lib.jk_predictor_with_learned(pred, learned, len(learned))
            if ranked:
                self._predictors.append(ranked)
                pred = ranked
        # The user's training words (spec §10.12), as the IME itself uses them.
        kw = str(settings.get("TrainingWords.Korean") or "").encode()
        ew = str(settings.get("TrainingWords.English") or "").encode()
        if (kw or ew) and "jk_predictor_with_words" in self.has:
            p = self.lib.jk_predictor_with_words(pred, kw, len(kw), ew, len(ew))
            if p:
                self._predictors.append(p)
                pred = p
        ka = str(settings.get("TrainingAvoid.Korean") or "").encode()
        ea = str(settings.get("TrainingAvoid.English") or "").encode()
        if (ka or ea) and "jk_predictor_with_avoided" in self.has:
            p = self.lib.jk_predictor_with_avoided(pred, ka, len(ka), ea, len(ea))
            if p:
                self._predictors.append(p)
                pred = p
        self.predictor = pred
        self.lib.jk_set_predictor(self.handle, pred)

    def _apply_settings(self, settings: dict) -> None:
        if "jk_set_recording" in self.has:
            self.lib.jk_set_recording(self.handle, 0)  # never record anything
        if not self.predictor:
            return
        if "jk_set_prediction_weight" in self.has:
            for i, name in enumerate(WEIGHT_NAMES):
                v = settings.get(f"Weight.{name}")
                try:
                    if v is not None:
                        self.lib.jk_set_prediction_weight(self.handle, i, float(v))
                except (TypeError, ValueError):
                    pass
        learned = settings.get("Learned")
        if isinstance(learned, str) and learned and "jk_learner_import" in self.has \
                and settings.get("LearnFromChoices"):
            b = learned.encode()
            if self.lib.jk_learner_import(self.handle, b, len(b)) and "jk_set_learning" in self.has:
                # Learned weights apply only while learning is on; recording stays off.
                self.lib.jk_set_learning(self.handle, 1)

    def close(self) -> None:
        with self._lock:
            if self.handle:
                self.lib.jk_free(self.handle)
                self.handle = None
            for p in reversed(self._predictors):
                self.lib.jk_predictor_free(p)
            self._predictors.clear()
            if self.dict:
                self.lib.jk_dict_free(self.dict)
                self.dict = None

    # ------------------------------------------------------------ modes
    @staticmethod
    def mode_for_source(input_source_id: str) -> tuple[int, str]:
        """(engine Korean mode id, layout name) for an input source id."""
        suffix = input_source_id.rsplit(".", 1)[-1] if input_source_id.startswith(BUNDLE_ID + ".") else "2Set"
        mode = MODES.get(suffix, 0)
        return KOREAN_MODE_FOR.get(mode, 0), suffix

    def set_mode(self, mode: int) -> None:
        if mode != self.mode:
            with self._lock:
                if self.lib.jk_set_mode(self.handle, mode):
                    self.mode = mode

    @staticmethod
    def ascii_to_presses(word: str, layout: str = "2Set") -> list[tuple[str, int]] | None:
        """Physical presses for ASCII text typed on the mode's English
        keyboard (letters and digits only): [(KeyboardEvent.code, shift)]."""
        rows = _LAYOUTS.get(layout)
        table: dict[str, str] = {}
        if rows:
            for r_layout, r_qwerty in zip(rows, _QWERTY):
                for a, b in zip(r_layout, r_qwerty):
                    table[a] = b
        out: list[tuple[str, int]] = []
        for ch in word:
            if ch.isascii() and ch.isalpha():
                q = table.get(ch.lower(), ch.lower())
                if not q.isalpha():
                    return None
                out.append(("Key" + q.upper(), 1 if ch.isupper() else 0))
            elif ch.isascii() and ch.isdigit():
                out.append(("Digit" + ch, 1))  # digits are on Shift in the Korean modes
            else:
                return None
        return out

    # ------------------------------------------------------------ reading
    def _type(self, presses: list[tuple[str, int]]) -> bool:
        self.lib.jk_clear(self.handle)
        for code, shift in presses:
            k = self.codes.get(code)
            if k is None:
                return False
            self.lib.jk_key(self.handle, k, 1 if shift else 0)
        return True

    def read(self, presses: list[tuple[str, int]]) -> Reading | None:
        with self._lock:
            if not self.handle or not self._type(presses):
                return None
            pre = self._str(self.lib.jk_preedit_ptr(self.handle), self.lib.jk_preedit_len(self.handle))
            eng = self._str(self.lib.jk_english_ptr(self.handle), self.lib.jk_english_len(self.handle))
            preds: list[Prediction] = []
            if self.predictor:
                n = self.lib.jk_predict(self.handle)
                for i in range(min(n, 8)):
                    preds.append(Prediction(
                        self._str(self.lib.jk_prediction_ptr(self.handle, i), self.lib.jk_prediction_len(self.handle, i)),
                        self.lib.jk_prediction_kind(self.handle, i)))
            self.lib.jk_clear(self.handle)
            return Reading(pre, eng, preds)

    def hanja(self, presses: list[tuple[str, int]], syllables: int, limit: int = 5) -> list[tuple[str, str]]:
        """Dictionary candidates (F7) for the whole word, if the dictionary has it."""
        if not self.dict:
            return []
        with self._lock:
            if not self.handle or not self._type(presses):
                return []
            f7 = self.codes.get("F7")
            out: list[tuple[str, str]] = []
            if f7 is not None:
                self.lib.jk_key(self.handle, f7, 0)
                for i in range(self.lib.jk_candidate_count(self.handle)):
                    h = self._str(self.lib.jk_candidate_ptr(self.handle, i), self.lib.jk_candidate_len(self.handle, i))
                    if len(h) != syllables:  # F7 converted only part of the word
                        continue
                    c = self._str(self.lib.jk_candidate_comment_ptr(self.handle, i),
                                  self.lib.jk_candidate_comment_len(self.handle, i))
                    out.append((h, c))
                    if len(out) >= limit:
                        break
            self.lib.jk_clear(self.handle)
            return out


def load(config: dict | None = None) -> JisKoreanEngine:
    """Finds and loads the engine; raises EngineUnavailable with a hint."""
    config = config or {}
    dylib = find_dylib(config.get("engine"))
    if not dylib:
        raise EngineUnavailable(
            "JIS Korean engine library not found. Build it with "
            "`cd ~/Projects/jis-korean/engine && cargo build --release`, or set "
            "JIS_KOREAN_ENGINE to libjis_korean.dylib.")
    predict, hanja = find_resources(config.get("resources"))
    settings = read_ime_defaults() if config.get("use_ime_settings", True) else {}
    return JisKoreanEngine(dylib, predict, hanja, settings)
