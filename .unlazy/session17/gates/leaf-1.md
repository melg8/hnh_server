# Gates: leaf-1 stale-jar-proof client launch

OWNS: windows/run-client.bat, windows/start-server.bat, windows/README.md

Scope: after `git pull`, run-client.bat must never silently run a stale
client jar. The bat rebuilds when HEAD moved (rev stamp) or the jar is
missing, and stamps the running rev into logs/client.log.

- [x] G1: run-client.bat no longer short-circuits on jar existence; it
  EVIDENCE: automatic-evidence=v1; definition-sha256=57895308cb069b39515c0de6e85deae5ee322672447c00ab3afea310169b2323; exit=0; EXPECT=matched; output-sha256=886f9c1c4da9e0398fe5de4135470930259f66cf613107a10a8e578a0c6c02e6; output-bytes=23; shell=/bin/sh; cwd=/home/z/my-project/workspace/hnh_server; path=cc94915413e1/11 entries
  contains the rev-stamp rebuild decision and writes the rev into the
  client log header
  CHECK: bash server/scripts/verify_windows_launch.sh leaf1-g1
  CWD: ../../..
  EXPECT: WINLAUNCH LEAF1 G1: OK
- [x] G2: the bat remains parseable and internally consistent: every
  EVIDENCE: automatic-evidence=v1; definition-sha256=3c0b01dd18c937c3936ae8667483e27de4531fcaa5db394b026ff25f0a5a0874; exit=0; EXPECT=matched; output-sha256=0acf699e464ae445145cd6f7e1a4d6d7b362cb61ea3b15d0ba02ac1d70111b10; output-bytes=23; shell=/bin/sh; cwd=/home/z/my-project/workspace/hnh_server; path=cc94915413e1/11 entries
  label/GOTO referenced exists, the rev-stamp block uses only cmd + git
  + ant
  CHECK: bash server/scripts/verify_windows_launch.sh leaf1-g2
  CWD: ../../..
  EXPECT: WINLAUNCH LEAF1 G2: OK
