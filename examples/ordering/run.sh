for i in {0..4}; do
    cargo run -- --my-id "$i" --ports 43000,43001,43002,43003,43004 --n-finalized 50 2> "node$i.log" &
done
