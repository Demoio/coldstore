#[allow(clippy::doc_lazy_continuation)]
pub mod common {
    tonic::include_proto!("coldstore.common");
}

#[allow(clippy::doc_lazy_continuation)]
pub mod metadata {
    tonic::include_proto!("coldstore.metadata");
}

#[allow(clippy::doc_lazy_continuation)]
pub mod scheduler {
    tonic::include_proto!("coldstore.scheduler");
}

#[allow(clippy::doc_lazy_continuation)]
pub mod cache {
    tonic::include_proto!("coldstore.cache");
}

#[allow(clippy::doc_lazy_continuation)]
pub mod tape {
    tonic::include_proto!("coldstore.tape");
}
