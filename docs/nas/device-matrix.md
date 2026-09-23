# NAS target inventory

Snapshot 2026-09-23. These are target candidates, not certified installations.
The 22-model editorial sample in the [plan](../nas-deployment-execution-plan.md)
is 20 x86-64 and two ARM64 by model count, with no sales weighting. Models below
the first table are additional coverage probes and do not change that count.

Recommendation sources and years: [2021/2022 two-bay](https://nascompares.com/2021/12/17/recommended-2-bay-nas-to-buy-in-2022/),
[2021 DS920+](https://www.androidcentral.com/synology-diskstation-ds920-plus-long-term-review),
[2022/2023 Plex](https://nascompares.com/2022/12/29/best-plex-nas-of-2023/amp/),
[2023/2024 Plex](https://nascompares.com/2023/12/22/the-best-plex-nas-of-2023-2024-a-buyers-guide/),
[2025 media NAS](https://nascompares.com/2025/12/26/best-plex-jellyfin-or-emby-nas-of-2025/),
and [2026 Plex](https://www.androidcentral.com/best-nas-plex).
Recommendation is evidence of editorial inclusion, not sales or installed base.

| Model | ISA, memory and OS family | Manufacturer hardware/deployment evidence | Recommendation year | Status |
| --- | --- | --- | --- | --- |
| Synology DS220j | ARM64, 512 MiB, DSM | [64-bit RTD1296 and RAM](https://www.synology.com/en-uk/store/Refurbished%20DS220j) | 2021/2022 | unassessed; low RAM may block server |
| Synology DS220+ | x86-64, DSM | [Synology CPU matrix](https://kb.synology.com/de-de/DSM/tutorial/What_kind_of_CPU_does_my_NAS_have) | 2021/2022 | unassessed |
| QNAP TS-253D | x86-64, QTS | [QNAP comparison](https://www.qnap.com/en-us/product/compare?products=ts-253d%2Chs-264%2Cts-453d%2Ctbs-464) | 2021/2022 | unassessed |
| Synology DS920+ | x86-64, DSM | [Synology CPU matrix](https://kb.synology.com/de-de/DSM/tutorial/What_kind_of_CPU_does_my_NAS_have) | 2021 | unassessed |
| QNAP TS-464 | x86-64, QTS | [QNAP spec](https://www.qnap.com/vi-vn/product/ts-464/specs/hardware/TS-464-8G.pdf) | 2022/2023 | unassessed |
| QNAP HS-264 | x86-64, QTS | [QNAP comparison](https://www.qnap.com/en-us/product/compare?products=ts-253d%2Chs-264%2Cts-453d%2Ctbs-464) | 2022/2023 | unassessed |
| ASUSTOR AS6704T | x86-64, ADM | [ASUSTOR hardware and Portainer](https://www.asustor.com//product?p_id=77) | 2022/2023 | unassessed |
| TerraMaster F4-423 | x86-64, TOS | [TerraMaster CPU](https://www.terra-master.com/pages/f4-423-specification) | 2022/2023 | unassessed |
| QNAP TVS-h874 | x86-64, QuTS hero | [QNAP architecture and Container Station](https://www.qnap.com/en-au/product/tvs-h874/specs/hardware) | 2022/2023 | unassessed |
| Synology DS224+ | x86-64, DSM | [Synology CPU](https://www.synology.com/en-br/products/DS224%2B) | 2023/2024 | unassessed |
| TerraMaster F2-424 | x86-64, TOS | [TerraMaster architecture](https://www.terra-master.com/de/pages/f2-424-specification) | 2023/2024 | unassessed |
| TerraMaster F4-424 | x86-64, TOS | [TerraMaster CPU](https://www.terra-master.com/pages/f4-424-specification) | 2023/2024 | unassessed |
| TerraMaster F4-424 Pro | x86-64, TOS | [TerraMaster CPU and Docker](https://www.terra-master.com/ja-jp/pages/f4-424-pro-specification) | 2023/2024 | unassessed |
| Synology BeeStation Plus | x86-64, BeeStation OS | [Intel J4125](https://global.download.synology.com/download/Document/Hardware/DataSheet/BeeStation/24-year/BST170-8T/enu/DataSheet_BST170-8T_enu.pdf); [Plex integration](https://bee.synology.com/en-au/BeeStation/Plus-Series) | 2025 | unassessed; arbitrary app install unverified |
| Minisforum N5 | x86-64, MinisCloud | [AMD Ryzen and Docker](https://www.minisforum.com/products/n5) | 2025 | unassessed |
| TerraMaster F4 SSD | x86-64, TOS | [Intel N95](https://www.terra-master.com/pages/f4-ssd-specification) | 2025 | unassessed |
| UGREEN DH4300 Plus | ARM64, 8 GiB, UGOS Pro | [ARM64 Docker support](https://ai.ugreen.com/blogs/knowledge/docker-docker-compose-ugreen-nas) | 2025 | unassessed; Rockchip backend absent |
| UGREEN DXP2800 | x86-64, UGOS Pro | [Intel N100](https://nas.ugreen.com/products/ugreen-nasync-dxp2800-nas-storage?SkipCozyRedirect=yes&from=nas-navi) | 2026 | unassessed |
| UGREEN DXP4800 Pro | x86-64, UGOS Pro | [Intel i3-1315U](https://nas-es.ugreen.com/products/dxp4800-pro) | 2026 | unassessed |
| ASUSTOR AS5402T | x86-64, ADM | [Intel N5105 and Portainer](https://www.asustor.com/de/product?p_id=81) | 2026 | unassessed |
| ZimaCube 2 | x86-64, ZimaOS | [Intel CPU and OS options](https://www.zimaspace.com/docs/hardware/) | 2026 | unassessed |
| Synology DS925+ | x86-64, DSM | [AMD Ryzen](https://www.synology.com/en-uk/products/DS925%2B) | 2026 | unassessed |

Additional candidates:

| Model | ISA / evidence | Installation evidence | Status |
| --- | --- | --- | --- |
| WD My Cloud EX2 Ultra WDBVBZ0120JCH-NESN | ARMv7 ARMADA 385; [Marvell ARMv7](https://www.marvell.com/products/infrastructure-processors/armada-38x.html) | [OS 5 eligible](https://support-en.wd.com/app/answers/detailweb/a_id/29230/~/devices-available-and-supported-for-my-cloud-os-5-firmware-upgrade); [manual apps](https://support-en.wd.com/app/answers/detailweb/a_id/29960/~/steps-to-download-and-install-third-party-apps-manually-on-my-cloud-os-5) | blocked: installed firmware, ABI and physical run unavailable |
| Synology DS223j | ARM64, 1 GiB; [Synology CPU matrix](https://kb.synology.com/de-de/DSM/tutorial/What_kind_of_CPU_does_my_NAS_have) | DSM Container Manager availability must be checked on exact firmware | unassessed |
| QNAP TS-233 | ARM64; [QNAP specification](https://www.qnap.com/en/product/ts-233/specs/hardware/TS-233.pdf) | QTS Container Station availability must be checked on exact firmware | unassessed |

## Appliance qualification fields

For each tested device, add a dated report with exact firmware and kernel,
userspace bitness/libc, RAM/swap, install route, GPU/render devices, server and
engine commits, image digest, direct play/remux/audio/video/HDR/subtitle verdicts,
latency, memory, errors, and reproduction steps. No such report exists yet.
