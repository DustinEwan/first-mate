# Changelog

## [0.1.2](https://github.com/DustinEwan/first-mate/compare/firstmate-v0.1.1...firstmate-v0.1.2) (2026-10-08)


### Features

* **content:** installer packages prompts; user dir overrides packaged ([c95b2a5](https://github.com/DustinEwan/first-mate/commit/c95b2a518a89543e26bf7db4a48e9a84f7401986))


### Bug Fixes

* **exec:** stop must reach a wedged PS session; sentinel cannot be clobbered ([dabce84](https://github.com/DustinEwan/first-mate/commit/dabce849fd06574563c132169da7ca2d0d49e92f))
* **prompt:** act through the OS, not around it; keep prompt content out of the binary ([333865c](https://github.com/DustinEwan/first-mate/commit/333865c8d4bd2009d00402c31f38cc038f4d2074))
* **prompt:** First Mate is a user at the desk, not a headless script ([ca11b62](https://github.com/DustinEwan/first-mate/commit/ca11b62787fc78ad99c45664469b7cd49a72973f))
* **prompt:** PowerShell session guidance from the wedged-session rollout ([90532b9](https://github.com/DustinEwan/first-mate/commit/90532b9a30776007a51acdc42507552786bcf91b))
* **prompt:** ship zero hardcoded prompt text; generalize OS-surface rule ([554600d](https://github.com/DustinEwan/first-mate/commit/554600dfd9ab0cd42de4ba2e52081953a48fcfc5))

## [0.1.1](https://github.com/DustinEwan/first-mate/compare/firstmate-v0.1.0...firstmate-v0.1.1) (2026-10-08)


### Features

* **providers:** data-driven provider roster matching omp breadth ([07b4b81](https://github.com/DustinEwan/first-mate/commit/07b4b810fe71978f39ab8cc179f57ec60b512ba6))
* **settings:** isLocal provider flag gates the Base URL field ([9fec638](https://github.com/DustinEwan/first-mate/commit/9fec6383d5442d2b0d42797ff204fa583ec6a9b5))
* **settings:** Proxy checkbox overrides hosted base URLs ([34abfa1](https://github.com/DustinEwan/first-mate/commit/34abfa16012c8237fa890b1913a26e9aaeb99e47))
* **ui:** console settings gear + anchor taskbar icon ([7c73719](https://github.com/DustinEwan/first-mate/commit/7c7371920bca0e44cde0599183815286075f572a))
* **update:** self-update via GitHub Releases; automated release pipeline ([32a42ee](https://github.com/DustinEwan/first-mate/commit/32a42ee9d25fde3031c3af01efc93f47b7e3baef))


### Bug Fixes

* **icons:** generate whole bundle from the anchor; drop dead set_main_icon ([98ff712](https://github.com/DustinEwan/first-mate/commit/98ff71246cd74c6692b2f43e9be6315e2f3e6461))
* **settings:** collapse empty label row above the Proxy checkbox ([0d50d52](https://github.com/DustinEwan/first-mate/commit/0d50d5298aa3ddfbb58c8923a376bb2ce1141a99))
* **window:** async open_settings command — sync build() deadlocked the app ([47900e9](https://github.com/DustinEwan/first-mate/commit/47900e9b8aeb39a1dcbbbb130b9d3151949b6aad))
