# eikon

**Гомоиконичное ядро: образ — это программа, программа — это образ.**

Ядро под RISC-V, загружается под OpenSBI в QEMU без своего бутлоадера, без крейтов, без аллокатора, без
MMU. Внутри — один тип значения: форма. Код — форма, состояние — форма, ошибка — форма, capability —
форма. Печать и чтение формы — интерфейс ядра, а не отладочный вывод.

Живое: **REPL по serial**, **компилятор в байткод** (в 2,1 раза быстрее обхода дерева на вызовах),
**генерации на диске** (virtio-blk, переживают перезагрузку), **прерывания и вытеснение**: таймер Sstc
забирает процессор каждую миллисекунду, каждая задача живёт на своём стеке, контракт (обещанный дедлайн
против фактического) считается после каждой задачи.

```text
$ tools/run.sh
eikon 0.1.0 - a homoiconic kernel
the image is the program; the program is the image
hart 0, riscv64, no mmu, no allocator, no crates
virtio-blk: 8192 sectors
image restored from disk: generation 1

=== boot self-test ===
[1] the image is text, and text is the image
  PASS boot image reads as eight forms
  PASS print -> read yields the same form
[2] the language runs: recursion, closures, let
  PASS (fact 10) = 3628800
  PASS (fib 15) = 610
  PASS (sq 7) = 49 (closure)
[3] the same forms, compiled: bytecode runs the boot image
  PASS compiled (fact 10) = 3628800
  PASS compiled (fib 15) = 610
  PASS compiled (sq 7) = 49
[4] state is a form: commit, audit, rollback
  PASS rollback republishes the old root
[5] authority is a capability, not a number
  PASS a forged handle is refused
[6] speed: tree-walk against bytecode, same forms
  (fib 12) x50 = 23k calls
    tree-walk : 124 ms, 187345 calls/s
    bytecode  : 58 ms, 395632 calls/s
[7] the image on disk: generations survive a reboot
  PASS what came back is what was saved
[8] preemption: the timer takes the CPU away
  PASS the timer fired and the tasks were interrupted
  PASS both tasks ran to completion
  PASS the long task was preempted more than once
  PASS a broken promise is counted: both kept and missed appear
  long: promised 5 ms, took 16 ms in 14 slices -> MISSED
  short: promised 1000 ms, took 0 ms -> kept
  timer ticks: 17, context switches: 14

=== PASS (failures: 0) ===
```

32 проверки, все зелёные: [`docs/boot-log.txt`](docs/boot-log.txt). Гейт одной командой: `tools/check.sh`
(собирает, загружает, падает, если самопроверка не прошла).

## Что уже работает

| | |
|---|---|
| **REPL** | `tools/repl.sh program.eikon` — формы читаются, исполняются, печатаются: `(sq 11) → 121`, `(commit …) → 6`, `(audit) → (5 3 2 1)`, ошибки как `! EINVAL: integer expected` |
| **Два движка** | обход дерева и компилятор в байткод; замыкание хранит и формы, и код, поэтому движки могут вызывать функции друг друга |
| **Скорость** | `(fib 12)`: 187k вызовов/с обходом, **396k вызовов/с** байткодом; `(+ 1 2)`: 1,35M форм/с у обоих (QEMU TCG, один hart) |
| **Поколения** | `commit` / `audit` / `rollback` / `root` / `gen` — старое поколение остаётся адресуемым |
| **Диск** | virtio-blk (legacy и modern): `(save)` пишет образ, следующая загрузка печатает `image restored from disk: generation N` |
| **Власть** | capability handle, выкованный секретом ядра; подделка отвергается |
| **Прерывание** | `stvec` + ассемблерный трамплин на 256 байт: 30 регистров, `sepc`, `sstatus`; таймер — Sstc/`stimecmp` или SBI |
| **Вытеснение** | у каждой задачи свой стек (1 МиБ), переключение контекста — это просто другой указатель стека; `(fib 17)` рвётся на 14 срезов по 1 мс |
| **Контракты** | `(spawn "имя" deadline-ms fn)` + `(sched-run)`: обещанное против фактического, нарушение считается и называется |

Диск пережил перезагрузку — `tools/persist.sh`, лог в [`docs/persist-demo.txt`](docs/persist-demo.txt):

```text
=== boot 1: commit a form and save the image ===
> (commit (list 'plant 42 7))
=> 6
> (save)
=> 12
=== boot 2: the same disk, nothing typed ===
virtio-blk: 8192 sectors
image restored from disk: generation 1
```

## Что это за штука

| Unix/Linux | eikon |
|---|---|
| программа компилируется в бинарь, данные живут отдельно | программа — это форма; то, что она печатает, читается обратно в ту же форму |
| состояние — изменяемые структуры и файлы | состояние — форма; изменение публикует новое поколение, старое остаётся адресуемым |
| права — uid и биты доступа | власть — capability handle, выкованный из секрета ядра |
| у каждой подсистемы свой формат (ioctl, sysfs, netlink) | один формат на всё: печатный текст форм |
| сборка мусора — коллектор с паузами | память — bump-арена; «сборка» = откат указателя после коммита |
| shell снаружи ядра | читатель и печататель форм — и есть интерфейс ядра |

Дух — Temple OS: одна машина, один язык, всё видно, код в RAM, никаких слоёв между программой и
железом. Не Temple OS — в коде: здесь есть capability-власть и неизменяемые поколения, потому что это
единственные две вещи, которые наши собственные работы (документное состояние, контракты, доказательства)
показали как необходимые.

## Сборка, запуск, доказательства

Нужны `rustc` с таргетом `riscv64gc-unknown-none-elf` и `qemu-system-riscv64`:

```text
rustup target add riscv64gc-unknown-none-elf
tools/run.sh              # собрать и загрузить
tools/check.sh            # загрузить и потребовать 'PASS (failures: 0)' (код возврата)
tools/repl.sh prog.eikon  # прогнать файл форм через REPL
tools/persist.sh          # две загрузки на одном диске: образ пережил перезагрузку
```

С диском: `-drive file=disk.img,format=raw,if=none,id=d0 -device virtio-blk-device,drive=d0`.
Ноль зависимостей: `[dependencies]` пуст, `cargo build --offline` проходит без предупреждений.

## Раскладка

```text
src/val.rs       значение и куча: арена, интернирование, mark/rewind
src/read.rs      текст -> форма
src/print.rs     форма -> текст (канонический: одно написание на значение)
src/eval.rs      обход дерева: форма на входе, форма на выходе
src/bytecode.rs  компилятор в линейный код + стековая VM
src/image.rs     поколения, commit/rollback/audit, capability
src/store.rs     образ на диске: печать формы в секторы и обратно
src/virtio.rs    virtio-mmio block: legacy и modern, опросом, без прерываний
src/sched.rs     контракты: EDF, замер обещания, счётчик нарушений
src/serial.rs    16550 UART: единственный драйвер ввода-вывода
src/main.rs      загрузка, самопроверка, REPL
```

## Чего здесь нет (честно)

- **MMU и адресных пространств**: адреса физические, изоляции процессов нет.
- **Приоритетов и справедливости**: планировщик — round-robin между невыполненными задачами, а не EDF;
  контракт по-прежнему *измеряется*, а не ограничивает исполнение (вытеснение есть, наказания за срыв нет).
- **Блокировок между задачами**: общее состояние (куча, образ) защищено только выключением прерываний на
  время записи, а не спинлоками; гонок нет потому, что hart один.
- Пользовательского режима, сети, чисел с плавающей точкой, строк переменной длины.
- Сборщика мусора: только арена и `rewind` (в неизменяемой модели это и есть сборка).
- Очередей ввода-вывода: один запрос за раз, опросом.

## Куда растёт

1. **Планировщик по дедлайнам** — заменить round-robin на EDF с пересчётом при каждом тике, чтобы
   контракт стал ограничением: опаздывающую задачу вытесняют, а не только считают.
2. **Пользовательский режим** — U-mode + capability вместо проверки uid.
3. **Сеть** — virtio-net тем же способом, что диск.
4. **Поколенческие арены** — освобождать арену целиком при коммите.

Лицензия MIT.
