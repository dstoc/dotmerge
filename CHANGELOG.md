# Changelog

## [0.1.1](https://github.com/dstoc/dotmerge/compare/dotmerge-v0.1.0...dotmerge-v0.1.1) (2026-06-05)


### Features

* `add` refuses to edit sync-critical commits ([564b6e9](https://github.com/dstoc/dotmerge/commit/564b6e90558facbeb33cd484c3b545879160bd61))
* add CLI and jj plumbing ([3979fd3](https://github.com/dstoc/dotmerge/commit/3979fd3c46a0697c4e93866ae4c4c2bbd42e72cc))
* add config module with file discovery and tilde expansion ([069a542](https://github.com/dstoc/dotmerge/commit/069a5429ec9f2a16b081db8d01a22d773fb86073))
* add paths relative to `cwd`, restrict to `home` ([214deca](https://github.com/dstoc/dotmerge/commit/214decac2b5a150394527a55a366cf0d6fe4a6a5))
* implement dotmerge add ([604d2ad](https://github.com/dstoc/dotmerge/commit/604d2ad0e4a5018b8997841d26c0290db8fa52c5))
* implement status and sync workflows ([d900e34](https://github.com/dstoc/dotmerge/commit/d900e34aea2fdb3d7b1bfa82fca7246639712ed8))
* implement the stubbed resume-precondition check ([a6e913a](https://github.com/dstoc/dotmerge/commit/a6e913aaaa10264e1d23f31d4c3ccd533e1e6771))
* load workspace once per command, thread 1 txn through sync ([67c3acb](https://github.com/dstoc/dotmerge/commit/67c3acb51910e8c5abf33bc89fa65f15e4fc7692))
* make export diff-aware and report written files ([30f9fce](https://github.com/dstoc/dotmerge/commit/30f9fce9095527b5149a4bef9deac8b988be9286))
* print a past-tense recap after sync ([2d1d21f](https://github.com/dstoc/dotmerge/commit/2d1d21faa8d9893a9caf44b64cdfc224c52224ae))
* reframe status around a single sync state ([dbd09ba](https://github.com/dstoc/dotmerge/commit/dbd09babe00af28852729a88917285f89e72ce10))
* replace full-content cleanliness scan with jj snapshot / metadata gating ([72dad7f](https://github.com/dstoc/dotmerge/commit/72dad7f1f1bd840ae21c72d3db2445991a676446))
* resolve home/repo/target from config file and flags ([46fe4c6](https://github.com/dstoc/dotmerge/commit/46fe4c613909d5b606a5c6ad62c7d069c66f9fb3))
* resume sync by refreshing the import under a prepared merge ([90907eb](https://github.com/dstoc/dotmerge/commit/90907eb22afcb289d427ceb865131819bea6a0fe))
* return typed outcomes from import and merge phases ([b8ee4f7](https://github.com/dstoc/dotmerge/commit/b8ee4f7366a7c309e87673f22d53aabd2286f9b9))


### Bug Fixes

* abandon redundant current-import state ([37eca31](https://github.com/dstoc/dotmerge/commit/37eca316a41e19c17e50551f66ac9af46502b414))
* author commits with the user's jj identity ([b83a244](https://github.com/dstoc/dotmerge/commit/b83a244dfb382155878a6fcabbda9dcfdf9fcdcd))
* avoid rewriting disposable empty @ during import ([88d4a98](https://github.com/dstoc/dotmerge/commit/88d4a980c6ae9ab3c39e2b72d776626bf9c97bd2))
* current-import is replaced, or becomes a child of @ ([2fadc34](https://github.com/dstoc/dotmerge/commit/2fadc344311324f5d4554e02ca799931dc468e92))
* drop disposable empty @ from sync history ([00dc045](https://github.com/dstoc/dotmerge/commit/00dc0453c8f7d0aa69498a39ca2971c9f53ccfff))
* export git refs ([d798507](https://github.com/dstoc/dotmerge/commit/d7985077acff5b0fa41c8981ed0018fb3c8df49b))
* persist the conflicted merge when sync stops on conflicts ([8548a2e](https://github.com/dstoc/dotmerge/commit/8548a2e74bcfac793c7512bdebaa4fb828da7df7))
* read hostname from gethostname(2), not $HOSTNAME ([bedd043](https://github.com/dstoc/dotmerge/commit/bedd043673070b6771f62f27d58efe7d13a0c1bc))
* reuse prepared state when target is already merged ([f75fcb0](https://github.com/dstoc/dotmerge/commit/f75fcb01a0666f4958b8c4818e74b3c3d05ae82d))
* treat ancestor targets as already applied in status ([6ef04e8](https://github.com/dstoc/dotmerge/commit/6ef04e84de0399c48f1c50e6f21be13b834adddb))
