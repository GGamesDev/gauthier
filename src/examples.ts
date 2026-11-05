export type TargetId = 'desktop' | 'pico' | 'pico2';

export interface Example {
  name: string;
  targets: TargetId[];
  code: string;
}

export const DESKTOP_TEMPLATE = `fn main() {
    println!("Hello from Gauthier!");
}
`;

export const PICO_TEMPLATE = `#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_time::Timer;
use picorust_runtime::println;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    // USB console + flashing without BOOTSEL. Keep this line first.
    picorust_runtime::init(spawner, p.USB).await;

    let mut led = Output::new(p.PIN_25, Level::Low);
    loop {
        led.toggle();
        println!("Blink!");
        Timer::after_millis(500).await;
    }
}
`;

export function templateFor(target: TargetId): string {
  return target === 'desktop' ? DESKTOP_TEMPLATE : PICO_TEMPLATE;
}

export const EXAMPLES: Example[] = [
  { name: 'Hello, world', targets: ['desktop'], code: DESKTOP_TEMPLATE },
  {
    name: 'Ask for your name',
    targets: ['desktop'],
    code: `use std::io;

fn main() {
    println!("What is your name?");
    let mut name = String::new();
    io::stdin().read_line(&mut name).expect("could not read the line");
    let name = name.trim();
    println!("Nice to meet you, {name}!");
}
`,
  },
  {
    name: 'Guess the number',
    targets: ['desktop'],
    code: `use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // A small random number without any external crate.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos();
    let secret = nanos % 100 + 1;
    println!("I picked a number between 1 and 100. Your guess?");

    let mut tries = 0;
    loop {
        let mut line = String::new();
        io::stdin().read_line(&mut line).unwrap();
        let guess: u32 = match line.trim().parse() {
            Ok(n) => n,
            Err(_) => {
                println!("Please type a number.");
                continue;
            }
        };
        tries += 1;
        if guess < secret {
            println!("Bigger!");
        } else if guess > secret {
            println!("Smaller!");
        } else {
            println!("Found it in {tries} tries!");
            break;
        }
    }
}
`,
  },
  {
    name: 'Structs and vectors',
    targets: ['desktop'],
    code: `#[derive(Debug)]
struct Planet {
    name: &'static str,
    moons: u32,
}

fn main() {
    let planets = vec![
        Planet { name: "Earth", moons: 1 },
        Planet { name: "Mars", moons: 2 },
        Planet { name: "Jupiter", moons: 95 },
    ];
    for p in &planets {
        println!("{} has {} moon(s)", p.name, p.moons);
    }
    let total: u32 = planets.iter().map(|p| p.moons).sum();
    println!("Total: {total}");
    println!("{:#?}", planets[0]);
}
`,
  },
  { name: 'Blink the LED', targets: ['pico', 'pico2'], code: PICO_TEMPLATE },
  {
    name: 'Talk to the Pico',
    targets: ['pico', 'pico2'],
    code: `#![no_std]
#![no_main]

use embassy_executor::Spawner;
use picorust_runtime::println;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    picorust_runtime::init(spawner, p.USB).await;

    println!("Type something in the console:");
    let mut buf = [0u8; 128];
    loop {
        let n = picorust_runtime::read_line(&mut buf).await;
        let text = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        println!("The Pico heard: {}", text);
    }
}
`,
  },
  {
    name: 'Button on GP15',
    targets: ['pico', 'pico2'],
    code: `#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use picorust_runtime::println;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    picorust_runtime::init(spawner, p.USB).await;

    // Wire a push button between GP15 and GND.
    let mut button = Input::new(p.PIN_15, Pull::Up);
    let mut led = Output::new(p.PIN_25, Level::Low);
    let mut presses = 0u32;
    loop {
        button.wait_for_falling_edge().await;
        presses += 1;
        led.toggle();
        println!("Button pressed {} time(s)", presses);
        button.wait_for_rising_edge().await;
    }
}
`,
  },
];
