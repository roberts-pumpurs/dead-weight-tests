# Changelog

## [0.2.2](https://github.com/roberts-pumpurs/dead-weight-tests/compare/v0.2.1...v0.2.2) (2026-10-10)


### Bug Fixes

* route copied review prompts to the skill's review path ([7ac6acd](https://github.com/roberts-pumpurs/dead-weight-tests/commit/7ac6acd6f956c537aa8a0a909873fbcacf81dd0d))

## [0.2.1](https://github.com/roberts-pumpurs/dead-weight-tests/compare/v0.2.0...v0.2.1) (2026-10-10)


### Bug Fixes

* name the dead-weight-tests package in the dwd install command ([#7](https://github.com/roberts-pumpurs/dead-weight-tests/issues/7)) ([eddcd51](https://github.com/roberts-pumpurs/dead-weight-tests/commit/eddcd5188116411c0eb3a4474832df48936a8729))

## [0.2.0](https://github.com/roberts-pumpurs/dead-weight-tests/compare/v0.1.1...v0.2.0) (2026-10-10)


### ⚠ BREAKING CHANGES

* the scripts under skills/dead-weight-tests/scripts are gone. Install dwd with `cargo install --locked --git https://github.com/roberts-pumpurs/dead-weight-tests`.

### Features

* clean up instrumented builds and validate review verdicts ([#3](https://github.com/roberts-pumpurs/dead-weight-tests/issues/3)) ([eb0aa0d](https://github.com/roberts-pumpurs/dead-weight-tests/commit/eb0aa0d276b274efb7543237bcad4a83c03873b9))
* rewrite the skill as the dwd Rust CLI ([#5](https://github.com/roberts-pumpurs/dead-weight-tests/issues/5)) ([612380f](https://github.com/roberts-pumpurs/dead-weight-tests/commit/612380fc3f28b29bbe00b0fdbed781e0a29c645b))


### Bug Fixes

* **release:** bump the dead-weight-tests version in Cargo.lock ([#6](https://github.com/roberts-pumpurs/dead-weight-tests/issues/6)) ([4154518](https://github.com/roberts-pumpurs/dead-weight-tests/commit/4154518e1c81df0c9bee002994aa1337c33a204f))

## [0.1.1](https://github.com/roberts-pumpurs/dead-weight-tests/compare/v0.1.0...v0.1.1) (2026-10-01)


### Bug Fixes

* **collect:** print only the usage comment for --help ([9e21aa2](https://github.com/roberts-pumpurs/dead-weight-tests/commit/9e21aa2dd11f2b008efc16795e9c5697f94e5bab))
* **collect:** rebuild workspace crates with coverage instead of reusing normal build artifacts ([7a4c2d4](https://github.com/roberts-pumpurs/dead-weight-tests/commit/7a4c2d45a83b40deea2f1dd0061c672a66b2a979))

## 0.1.0 (2026-10-01)


### Features

* dead-weight-tests agent skill ([1b7fa4c](https://github.com/roberts-pumpurs/dead-weight-tests/commit/1b7fa4cb29d08ccc85d465cbc3a87df1491ec498))
