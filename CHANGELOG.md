# Changelog

## [0.1.2](https://github.com/DustinEwan/first-mate/compare/firstmate-v0.1.1...firstmate-v0.1.2) (2026-10-13)


### Features

* **audit:** hold a flagged message until you decide ([5a8dbcd](https://github.com/DustinEwan/first-mate/commit/5a8dbcd1eca4756c6723e4012043afbf5213d489))
* **content:** installer packages prompts; user dir overrides packaged ([c95b2a5](https://github.com/DustinEwan/first-mate/commit/c95b2a518a89543e26bf7db4a48e9a84f7401986))
* **context:** fit long conversations to the model's window ([4613bf2](https://github.com/DustinEwan/first-mate/commit/4613bf28178cdcf40788906724ead3932a01b32d))
* **crypto:** seal every store file with a TPM-wrapped key ([ebc83e0](https://github.com/DustinEwan/first-mate/commit/ebc83e0bdf2f748e3bd9faa849ebd979bab93dd0))
* **decide:** judge the whole message in overlapping windows ([f357cf3](https://github.com/DustinEwan/first-mate/commit/f357cf3a509c5cd1dab5d691df96009d2b5ffba3))
* **elevation:** opt-in elevated control via UAC-consented helper ([72da9a3](https://github.com/DustinEwan/first-mate/commit/72da9a32ab02f1dbdd946508ab3679ceb8c4022b))
* **leak:** mark every leaked value and export the record ([cc97fc0](https://github.com/DustinEwan/first-mate/commit/cc97fc01bd44519c7b047088a75c299c3fb1b4db))
* **prompts:** make First Mate report intent and outcome, not the route ([dc9b782](https://github.com/DustinEwan/first-mate/commit/dc9b7826903d31dffac8746f2cf221f541b1ace7))
* **prompts:** package the compaction brief rules as content ([6b25468](https://github.com/DustinEwan/first-mate/commit/6b254686cdf48290f8df78f1aecf44e5f2079e2d))
* **redact:** answer the leak report from the transcripts ([ed863cf](https://github.com/DustinEwan/first-mate/commit/ed863cf6769fc72c798789aadb68f356ee51f70b))
* **redact:** gate each span on a decision server you run ([062bc49](https://github.com/DustinEwan/first-mate/commit/062bc4911ff1be4a813451b33517572bbc907adc))
* **redact:** keep one sealed token map across runs ([91c7dcc](https://github.com/DustinEwan/first-mate/commit/91c7dcced58e095317f6408255888f72b89591bc))
* **redact:** replace each detected secret with a reversible token ([8d8be7e](https://github.com/DustinEwan/first-mate/commit/8d8be7e934ea655c3e79a7fa811e02ff5b8451f9))
* **redact:** run the pass at the wire boundary of both chat paths ([72ccc9c](https://github.com/DustinEwan/first-mate/commit/72ccc9c1bab9f61b30fcb63c6338a99a04e76ce6))
* **settings:** block Save until the Privacy Model acknowledgement ([db6912a](https://github.com/DustinEwan/first-mate/commit/db6912adc138b7fdf872c1f6c3c1607b93532909))
* **settings:** require the acknowledgement before a remote scorer is saved ([194a9a5](https://github.com/DustinEwan/first-mate/commit/194a9a57601d6b7583099a552b892f0ae9feff70))
* **settings:** show the Privacy Model acknowledgement for any host ([f62fff8](https://github.com/DustinEwan/first-mate/commit/f62fff8419d9b7042f8e934cb18841e267723240))
* **settings:** split settings into a left-hand category rail ([1d7fdfa](https://github.com/DustinEwan/first-mate/commit/1d7fdfa92afc8bcd2bc6bbe1d3eda43d5f708cb4))
* **setup:** gate setup on the recovery key, and speak to the user ([e8b0029](https://github.com/DustinEwan/first-mate/commit/e8b0029ad6407511fb7db64745633e61365f6a69))
* **transcript:** store the wire copy, and open a fold as evidence ([48848a4](https://github.com/DustinEwan/first-mate/commit/48848a4865982e28332f6b3cf323bceeb6914391))
* **ui:** add the provider switch and the redaction chip ([121d41f](https://github.com/DustinEwan/first-mate/commit/121d41f9c5e8b4db3ed565134d09efb5df8e2bba))


### Bug Fixes

* **chat:** pin scroll to bottom until DynamicScroller sizes settle ([870f96b](https://github.com/DustinEwan/first-mate/commit/870f96bf92b807b4b34354933219a6fd7eab3482))
* **console:** stop sending the user to the debug log ([fdc1f67](https://github.com/DustinEwan/first-mate/commit/fdc1f677bb99ab06736069d5c9d715716e4e3743))
* **context:** treat an unknown context window as no window ([656b83c](https://github.com/DustinEwan/first-mate/commit/656b83cf4b7fc79ebbd36cbf3f053db1435f88db))
* **crypto:** create the recovery file by rename ([2598c47](https://github.com/DustinEwan/first-mate/commit/2598c47d4cd309566f61fd6539739efad4eec5ae))
* **crypto:** keep the recovery key pending until the user says they kept it ([1cec328](https://github.com/DustinEwan/first-mate/commit/1cec328fc990df033d7fc719711ae32872b40ecf))
* **exec:** stop must reach a wedged PS session; sentinel cannot be clobbered ([dabce84](https://github.com/DustinEwan/first-mate/commit/dabce849fd06574563c132169da7ca2d0d49e92f))
* **harness:** stop the duplicate gate and stale handles from costing turns ([85e6e10](https://github.com/DustinEwan/first-mate/commit/85e6e10afdb47cc90b1ccf0d1d2db47242a8575a))
* **log:** write the debug log only in a debug build ([a6271cd](https://github.com/DustinEwan/first-mate/commit/a6271cd1443e0308f9f0f894108f51d431e791f4))
* **prompt:** act through the OS, not around it; keep prompt content out of the binary ([333865c](https://github.com/DustinEwan/first-mate/commit/333865c8d4bd2009d00402c31f38cc038f4d2074))
* **prompt:** First Mate is a user at the desk, not a headless script ([ca11b62](https://github.com/DustinEwan/first-mate/commit/ca11b62787fc78ad99c45664469b7cd49a72973f))
* **prompt:** PowerShell session guidance from the wedged-session rollout ([90532b9](https://github.com/DustinEwan/first-mate/commit/90532b9a30776007a51acdc42507552786bcf91b))
* **prompt:** ship zero hardcoded prompt text; generalize OS-surface rule ([554600d](https://github.com/DustinEwan/first-mate/commit/554600dfd9ab0cd42de4ba2e52081953a48fcfc5))
* **redact:** score no authored text, and score the brief before the fold ([27185d5](https://github.com/DustinEwan/first-mate/commit/27185d557378063de8b081862f8ec6d09facd397))
* **redact:** stop a path value where the prose starts ([2686c11](https://github.com/DustinEwan/first-mate/commit/2686c117231616ce178889a028efbaf30aed6820))
* **settings:** name the dial and the score in the threshold field ([7f4d647](https://github.com/DustinEwan/first-mate/commit/7f4d6473228a8466b5ef53241cf134b6bed58d10))
* **settings:** name the second switch with the noun from the first ([01e0781](https://github.com/DustinEwan/first-mate/commit/01e0781ab6efde6e8e8c106d111073b5e2605c6a))
* **settings:** point the second privacy switch at the first ([49bff57](https://github.com/DustinEwan/first-mate/commit/49bff573c64602e3c879e955bb6f199064af143f))
* **settings:** put the exposure rule on the machine, not the service ([d542e2f](https://github.com/DustinEwan/first-mate/commit/d542e2f0204ca0f515c5510f80ff8456a3eeaa75))
* **settings:** refetch provider settings when setup finishes ([8bfb094](https://github.com/DustinEwan/first-mate/commit/8bfb094388e5e40d7cfb64a7f28d138089c00b77))
* **settings:** scroll the option list, not the whole window ([ae5a7fe](https://github.com/DustinEwan/first-mate/commit/ae5a7fe3e0f7fef761c667e0f34c3e4230f4fba1))
* **settings:** size every gap the same way ([5ff9549](https://github.com/DustinEwan/first-mate/commit/5ff9549dae831033f7f1570b11dd2156ca4b9bce))
* **settings:** space the settings so feedback reads with its control ([9d7bcd1](https://github.com/DustinEwan/first-mate/commit/9d7bcd1e4d315794cd5e9cf507c2b1d0d464985d))
* **settings:** state the privacy copy as what changes, not how ([22247a4](https://github.com/DustinEwan/first-mate/commit/22247a4fa3f8127fe3fa490e153a4cf7818c31a4))
* **settings:** state the remote Privacy Model risk in plain terms ([f23e9af](https://github.com/DustinEwan/first-mate/commit/f23e9af82ab9bf9b22c1fa70475aa913e6dad895))
* **settings:** state the second-model copy as what the user gains ([38ac3bc](https://github.com/DustinEwan/first-mate/commit/38ac3bcd0c57070230eaff4d5c0a94e4cc0ddb7c))
* **settings:** state what each privacy field decides ([066cbf6](https://github.com/DustinEwan/first-mate/commit/066cbf6f79963372d4a7031affa6b03a9c84fe6c))
* **settings:** stop claiming a password detector exists ([53afbf6](https://github.com/DustinEwan/first-mate/commit/53afbf640e51504807ccf0f6f02b9a0df3af26fc))
* **skills:** spec-pure skill content, skill-gated tool dispatch ([89d295a](https://github.com/DustinEwan/first-mate/commit/89d295a069a537b0f0ad32ba60c46074a8efe376))
* **ui:** mark each leaked value where the value sits ([2b61199](https://github.com/DustinEwan/first-mate/commit/2b611996447c19c041bba075fcd396448a031769))

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
