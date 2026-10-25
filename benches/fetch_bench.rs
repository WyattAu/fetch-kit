use criterion::{criterion_group, criterion_main, Criterion};
use resilient_fetch::ClientBuilder;
use std::time::Duration;

fn bench_client_builder_default(c: &mut Criterion) {
    c.bench_function("client_builder_default", |b| {
        b.iter(|| {
            let builder = ClientBuilder::new();
            std::hint::black_box(builder);
        });
    });
}

fn bench_client_builder_with_options(c: &mut Criterion) {
    c.bench_function("client_builder_with_options", |b| {
        b.iter(|| {
            let builder = ClientBuilder::new()
                .base_url("https://api.example.com")
                .timeout(Duration::from_secs(10))
                .retries(5);
            std::hint::black_box(builder);
        });
    });
}

fn bench_client_build(c: &mut Criterion) {
    c.bench_function("client_build", |b| {
        b.iter(|| {
            let client = ClientBuilder::new().build();
            std::hint::black_box(client);
        });
    });
}

fn bench_client_build_with_base_url(c: &mut Criterion) {
    c.bench_function("client_build_with_base_url", |b| {
        b.iter(|| {
            let client = ClientBuilder::new()
                .base_url("https://api.example.com")
                .build();
            std::hint::black_box(client);
        });
    });
}

fn bench_client_builder_chaining(c: &mut Criterion) {
    c.bench_function("client_builder_chaining", |b| {
        b.iter(|| {
            let client = ClientBuilder::new()
                .base_url("https://api.example.com")
                .timeout(Duration::from_secs(60))
                .retries(10)
                .build();
            std::hint::black_box(client);
        });
    });
}

criterion_group!(
    benches,
    bench_client_builder_default,
    bench_client_builder_with_options,
    bench_client_build,
    bench_client_build_with_base_url,
    bench_client_builder_chaining,
);
criterion_main!(benches);
