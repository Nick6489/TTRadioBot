`tone.mp3` is a generated 440 Hz sine wave, with no third-party recording.
Recreate with FFmpeg (used only to produce this test fixture, never by the bot):

```
ffmpeg -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=1' -ac 2 -codec:a libmp3lame -b:a 64k -write_xing 0 -id3v2_version 0 tone.mp3
```
