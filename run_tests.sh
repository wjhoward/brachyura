cargo build
RUST_LOG=info cargo run &
PID1=$(echo $!)
echo " "
sleep 1
python3 test_server.py &
PID2=$(echo $!)
echo " "
sleep 1
python3 test_proxy.py
kill $PID1 $PID2