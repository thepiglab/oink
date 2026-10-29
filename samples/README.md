# Sample recordings

Two recordings are built into Oink, so anyone can see one straight away: pick one on the start
screen, or run `oink --sample` (resting EEG) or `oink --sample motor` (motor imagery).

## Subject00_1.edf: resting EEG

| | |
|---|---|
| Dataset | *EEG During Mental Arithmetic Tasks*, version 1.0.0, PhysioNet: https://physionet.org/content/eegmat/1.0.0/ |
| Recording | Subject 0, background EEG before the task: 3 min 2 s of eyes-closed rest. 19 EEG channels (10-20 system, linked-ear reference), A2–A1, ECG; 500 Hz; EDF+. The authors removed artefacts with ICA. |
| License | [Open Data Commons Attribution License v1.0](https://opendatacommons.org/licenses/by/1-0/) (ODC-By). You may use, share and adapt it, commercially too, as long as you credit the source. |
| Integrity | Unmodified. SHA-256 `8b23c443f2f5f733f8ccc31a54377898009c28c7563d1107692a8735212d2f15` matches the dataset's `SHA256SUMS.txt`. |

### Credit

Zyma I, Tukaev S, Seleznov I, Kiyono K, Popov A, Chernykh M, Shpenkov O. Electroencephalograms
during Mental Arithmetic Task Performance. *Data*. 2019; 4(1):14. https://doi.org/10.3390/data4010014

Distributed by PhysioNet: Pollard T, et al. PhysioNet as a global platform for biomedical research.
*Nature Health* (2026). https://doi.org/10.1038/s44360-026-00096-z

## S001R04.edf: motor imagery, with labelled events

| | |
|---|---|
| Dataset | *EEG Motor Movement/Imagery Dataset*, version 1.0.0, PhysioNet: https://physionet.org/content/eegmmidb/1.0.0/ |
| Recording | Subject 1, run 4: imagined opening and closing of the left or right fist, cued by a target on the left or right of a screen. 2 min 5 s; 64 EEG channels (10-10 system), 160 Hz; EDF+, recorded with BCI2000. |
| Events | 30 annotations in the file itself, back to back, about 4.1 s each: `T0` rest (15), `T1` imagined left fist (8), `T2` imagined right fist (7). The file does not say what the codes mean; the dataset's documentation does, by run number (runs 4, 8 and 12 are left/right fist imagery). |
| License | [Open Data Commons Attribution License v1.0](https://opendatacommons.org/licenses/by/1-0/) (ODC-By). |
| Integrity | Unmodified. SHA-256 `3d161f88e1c00632585287d2ce584c2bc0f08862438eb255ea8723e00fac693d` matches the dataset's `SHA256SUMS.txt`. |

### Credit

Schalk G, McFarland DJ, Hinterberger T, Birbaumer N, Wolpaw JR. BCI2000: A General-Purpose
Brain-Computer Interface (BCI) System. *IEEE Transactions on Biomedical Engineering*. 2004;
51(6):1034-1043.

Schalk G. EEG Motor Movement/Imagery Dataset (version 1.0.0). PhysioNet (2009).
https://doi.org/10.13026/C28G6P

Distributed by PhysioNet: Pollard T, et al. PhysioNet as a global platform for biomedical research.
*Nature Health* (2026). https://doi.org/10.1038/s44360-026-00096-z
