
### rendered 800 px wide; the screen waits for the GPU to finish each frame (lavapipe's raster is the frame)

| row | FFmpeg | drawn of 60 | last frame on screen (ms) | request to screen p50 / p95 (ms) | per drawn frame (ms) | inputs taken |
| --- | --- | --- | --- | --- | --- | --- |
| steady, inputs taken per frame | 6.1.1 | 60 | 5507 (5312 to 5888) | 88.1 / 133.3 | 91.8 (88.5 to 98.1) | 60 |
| steady, inputs taken per frame | 9.0.2 | 60 | 5618 (4962 to 5872) | 90.8 / 127.6 | 93.6 (82.7 to 97.9) | 60 |
| steady, held snapshot | 6.1.1 | 60 | 5945 (5382 to 5964) | 95.8 / 136.1 | 99.1 (89.7 to 99.4) | 3 |
| steady, held snapshot | 9.0.2 | 60 | 5208 (4052 to 5987) | 87.1 / 116.8 | 86.8 (67.5 to 99.8) | 3 |
| before: page single-flight, pointer 125 Hz | 6.1.1 | 7 | 647 (645 to 715) | 115.5 / 153.9 | 102.2 (92.1 to 129.5) | 7 |
| before: page single-flight, pointer 125 Hz | 9.0.2 | 5 | 676 (613 to 779) | 140.4 / 174.5 | 135.2 (87.6 to 155.7) | 5 |
| after:  mailbox, pointer 125 Hz | 6.1.1 | 5 | 774 (725 to 881) | 204.4 / 282.4 | 176.1 (145.1 to 193.5) | 1 |
| after:  mailbox, pointer 125 Hz | 9.0.2 | 8 | 648 (648 to 734) | 99.5 / 126.8 | 81.0 (81.0 to 146.7) | 1 |
| before: page single-flight, pointer 60 Hz | 6.1.1 | 11 | 1160 (1127 to 1185) | 111.5 / 168.0 | 105.5 (91.2 to 125.2) | 11 |
| before: page single-flight, pointer 60 Hz | 9.0.2 | 13 | 1070 (1064 to 1214) | 87.3 / 105.1 | 82.3 (66.5 to 121.4) | 13 |
| after:  mailbox, pointer 60 Hz | 6.1.1 | 9 | 1205 (1161 to 1258) | 145.8 / 228.5 | 133.9 (96.8 to 139.8) | 1 |
| after:  mailbox, pointer 60 Hz | 9.0.2 | 10 | 1108 (1067 to 1215) | 116.4 / 151.0 | 110.8 (76.2 to 121.5) | 1 |

### rendered 800 px wide; CPU side only: the GPU's work is not waited for, as a real present does not

| row | FFmpeg | drawn of 60 | last frame on screen (ms) | request to screen p50 / p95 (ms) | per drawn frame (ms) | inputs taken |
| --- | --- | --- | --- | --- | --- | --- |
| steady, inputs taken per frame | 6.1.1 | 60 | 5462 (4932 to 6233) | 84.9 / 204.5 | 91.0 (82.2 to 103.9) | 60 |
| steady, inputs taken per frame | 9.0.2 | 60 | 4462 (3671 to 5722) | 79.1 / 171.5 | 74.4 (61.2 to 95.4) | 60 |
| steady, held snapshot | 6.1.1 | 60 | 5894 (5859 to 5997) | 94.1 / 220.8 | 98.2 (97.7 to 100.0) | 3 |
| steady, held snapshot | 9.0.2 | 60 | 3871 (3683 to 5425) | 66.8 / 147.6 | 64.5 (61.4 to 90.4) | 2 |
| before: page single-flight, pointer 125 Hz | 6.1.1 | 6 | 735 (727 to 847) | 169.4 / 234.6 | 141.1 (122.5 to 145.4) | 6 |
| before: page single-flight, pointer 125 Hz | 9.0.2 | 8 | 620 (601 to 683) | 98.7 / 173.7 | 77.5 (75.9 to 100.1) | 8 |
| after:  mailbox, pointer 125 Hz | 6.1.1 | 5 | 891 (717 to 937) | 222.9 / 398.5 | 178.1 (143.4 to 187.4) | 1 |
| after:  mailbox, pointer 125 Hz | 9.0.2 | 7 | 629 (588 to 659) | 124.3 / 161.4 | 94.2 (78.6 to 117.7) | 1 |
| before: page single-flight, pointer 60 Hz | 6.1.1 | 9 | 1163 (1141 to 1238) | 164.9 / 192.9 | 129.2 (95.0 to 154.7) | 9 |
| before: page single-flight, pointer 60 Hz | 9.0.2 | 12 | 1118 (1044 to 1323) | 127.3 / 242.5 | 110.3 (69.6 to 111.8) | 12 |
| after:  mailbox, pointer 60 Hz | 6.1.1 | 9 | 1278 (1206 to 1322) | 120.6 / 293.7 | 142.0 (100.5 to 146.9) | 1 |
| after:  mailbox, pointer 60 Hz | 9.0.2 | 12 | 1151 (1113 to 1267) | 121.8 / 237.3 | 95.9 (65.5 to 115.2) | 1 |

### rendered 400 px wide over a gap in the cut (no layer: nothing to decode, the cheapest frame the path draws)

| row | FFmpeg | drawn of 60 | last frame on screen (ms) | request to screen p50 / p95 (ms) | per drawn frame (ms) | inputs taken |
| --- | --- | --- | --- | --- | --- | --- |
| steady, inputs taken per frame | 6.1.1 | 60 | 282 (255 to 282) | 4.3 / 7.3 | 4.7 (4.2 to 4.7) | 60 |
| steady, inputs taken per frame | 9.0.2 | 60 | 303 (233 to 327) | 4.6 / 9.5 | 5.0 (3.9 to 5.5) | 60 |
| steady, held snapshot | 6.1.1 | 60 | 231 (192 to 287) | 3.6 / 6.7 | 3.9 (3.2 to 4.8) | 1 |
| steady, held snapshot | 9.0.2 | 60 | 271 (218 to 402) | 3.8 / 7.7 | 4.5 (3.6 to 6.7) | 1 |
| before: page single-flight, pointer 125 Hz | 6.1.1 | 58 | 490 (485 to 490) | 5.2 / 12.6 | 8.5 (8.1 to 9.2) | 58 |
| before: page single-flight, pointer 125 Hz | 9.0.2 | 58 | 499 (489 to 511) | 8.0 / 12.4 | 8.6 (8.2 to 9.5) | 58 |
| after:  mailbox, pointer 125 Hz | 6.1.1 | 60 | 488 (487 to 494) | 6.4 / 9.9 | 8.1 (8.1 to 8.2) | 1 |
| after:  mailbox, pointer 125 Hz | 9.0.2 | 60 | 496 (488 to 518) | 5.3 / 9.6 | 8.3 (8.1 to 8.6) | 1 |
| before: page single-flight, pointer 60 Hz | 6.1.1 | 60 | 958 (956 to 960) | 4.2 / 7.4 | 16.0 (15.9 to 16.0) | 60 |
| before: page single-flight, pointer 60 Hz | 9.0.2 | 60 | 967 (956 to 975) | 5.2 / 9.8 | 16.1 (15.9 to 16.2) | 60 |
| after:  mailbox, pointer 60 Hz | 6.1.1 | 60 | 966 (960 to 972) | 7.4 / 10.3 | 16.1 (16.0 to 16.2) | 1 |
| after:  mailbox, pointer 60 Hz | 9.0.2 | 60 | 967 (961 to 974) | 6.2 / 9.8 | 16.1 (16.0 to 16.2) | 1 |

### rendered 400 px wide over a gap in the cut, in a project of 120 clips on four tracks (the plan is made from all of them)

| row | FFmpeg | drawn of 60 | last frame on screen (ms) | request to screen p50 / p95 (ms) | per drawn frame (ms) | inputs taken |
| --- | --- | --- | --- | --- | --- | --- |
| steady, inputs taken per frame | 6.1.1 | 60 | 255 (182 to 261) | 3.9 / 7.6 | 4.2 (3.0 to 4.3) | 60 |
| steady, inputs taken per frame | 9.0.2 | 60 | 229 (223 to 234) | 3.5 / 7.0 | 3.8 (3.7 to 3.9) | 60 |
| steady, held snapshot | 6.1.1 | 60 | 157 (152 to 225) | 2.2 / 5.8 | 2.6 (2.5 to 3.8) | 1 |
| steady, held snapshot | 9.0.2 | 60 | 177 (173 to 247) | 2.8 / 5.7 | 2.9 (2.9 to 4.1) | 1 |
| before: page single-flight, pointer 125 Hz | 6.1.1 | 60 | 485 (484 to 488) | 3.6 / 9.0 | 8.1 (8.1 to 8.1) | 60 |
| before: page single-flight, pointer 125 Hz | 9.0.2 | 60 | 492 (489 to 503) | 4.6 / 9.0 | 8.2 (8.1 to 8.5) | 60 |
| after:  mailbox, pointer 125 Hz | 6.1.1 | 60 | 485 (483 to 486) | 5.2 / 8.3 | 8.1 (8.1 to 8.1) | 1 |
| after:  mailbox, pointer 125 Hz | 9.0.2 | 60 | 492 (490 to 499) | 4.1 / 8.7 | 8.3 (8.2 to 8.3) | 1 |
| before: page single-flight, pointer 60 Hz | 6.1.1 | 60 | 959 (956 to 959) | 4.1 / 5.9 | 16.0 (15.9 to 16.0) | 60 |
| before: page single-flight, pointer 60 Hz | 9.0.2 | 60 | 964 (957 to 973) | 4.2 / 8.5 | 16.1 (16.0 to 16.2) | 60 |
| after:  mailbox, pointer 60 Hz | 6.1.1 | 60 | 963 (959 to 968) | 3.4 / 7.9 | 16.0 (16.0 to 16.1) | 1 |
| after:  mailbox, pointer 60 Hz | 9.0.2 | 60 | 966 (964 to 972) | 4.9 / 8.6 | 16.1 (16.1 to 16.2) | 1 |