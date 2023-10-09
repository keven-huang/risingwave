#!/usr/bin/env bash

# Exits as soon as any line fails.
set -euo pipefail

source ci/scripts/common.sh

# prepare environment
export CONNECTOR_RPC_ENDPOINT="localhost:50051"
export CONNECTOR_LIBS_PATH="./connector-node/libs"

while getopts 'p:' opt; do
    case ${opt} in
        p )
            profile=$OPTARG
            ;;
        \? )
            echo "Invalid Option: -$OPTARG" 1>&2
            exit 1
            ;;
        : )
            echo "Invalid option: $OPTARG requires an argument" 1>&2
            ;;
    esac
done
shift $((OPTIND -1))

download_and_prepare_rw "$profile" source

echo "--- Download connector node package"
buildkite-agent artifact download risingwave-connector.tar.gz ./
mkdir ./connector-node
tar xf ./risingwave-connector.tar.gz -C ./connector-node

echo "--- Prepare data"
cp src/connector/src/test_data/simple-schema.avsc ./avro-simple-schema.avsc
cp src/connector/src/test_data/complex-schema.avsc ./avro-complex-schema.avsc
cp src/connector/src/test_data/complex-schema ./proto-complex-schema
cp src/connector/src/test_data/complex-schema.json ./json-complex-schema


echo "--- e2e, ci-1cn-1fe, mysql & postgres cdc"

# import data to mysql
mysql --host=mysql --port=3306 -u root -p123456 < ./e2e_test/source/cdc/mysql_cdc.sql

# import data to postgres
export PGHOST=db PGUSER=postgres PGPASSWORD=postgres PGDATABASE=cdc_test
createdb
psql < ./e2e_test/source/cdc/postgres_cdc.sql

node_port=50051
node_timeout=10

wait_for_connector_node_start() {
  start_time=$(date +%s)
  while :
  do
      if nc -z localhost $node_port; then
          echo "Port $node_port is listened! Connector Node is up!"
          break
      fi

      current_time=$(date +%s)
      elapsed_time=$((current_time - start_time))
      if [ $elapsed_time -ge $node_timeout ]; then
          echo "Timeout waiting for port $node_port to be listened!"
          exit 1
      fi
      sleep 0.1
  done
  sleep 2
}

echo "--- starting risingwave cluster with connector node"
RUST_LOG="info,risingwave_stream=info,risingwave_batch=info,risingwave_storage=info" \
cargo make ci-start ci-1cn-1fe-with-recovery
./connector-node/start-service.sh -p $node_port > .risingwave/log/connector-node.log 2>&1 &

echo "waiting for connector node to start"
wait_for_connector_node_start

echo "--- inline cdc test"
sqllogictest -p 4566 -d dev './e2e_test/source/cdc_inline/**/*.slt'

echo "--- mysql & postgres cdc validate test"
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.validate.mysql.slt'
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.validate.postgres.slt'

echo "--- mysql & postgres load and check"
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.load.slt'
# wait for cdc loading
sleep 10
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.check.slt'

# cdc share stream test cases
export MYSQL_HOST=mysql MYSQL_TCP_PORT=3306 MYSQL_PWD=123456
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.share_stream.slt'


# kill cluster and the connector node
cargo make kill
pkill -f connector-node
echo "cluster killed "

# insert new rows
mysql --host=mysql --port=3306 -u root -p123456 < ./e2e_test/source/cdc/mysql_cdc_insert.sql
psql < ./e2e_test/source/cdc/postgres_cdc_insert.sql
echo "inserted new rows into mysql and postgres"

# start cluster w/o clean-data
RUST_LOG="info,risingwave_stream=info,risingwave_batch=info,risingwave_storage=info" \
touch .risingwave/log/connector-node.log
./connector-node/start-service.sh -p $node_port >> .risingwave/log/connector-node.log 2>&1 &
echo "(recovery) waiting for connector node to start"
wait_for_connector_node_start

cargo make dev ci-1cn-1fe-with-recovery
echo "wait for cluster recovery finish"
sleep 20
echo "check mviews after cluster recovery"
# check results
sqllogictest -p 4566 -d dev './e2e_test/source/cdc/cdc.check_new_rows.slt'

echo "--- Kill cluster"
cargo make ci-kill
pkill -f connector-node

echo "--- e2e, ci-1cn-1fe, protobuf schema registry"
RUST_LOG="info,risingwave_stream=info,risingwave_batch=info,risingwave_storage=info" \
cargo make ci-start ci-1cn-1fe
python3 -m pip install requests protobuf confluent-kafka
python3 e2e_test/schema_registry/pb.py "message_queue:29092" "http://message_queue:8081" "sr_pb_test" 20
sqllogictest -p 4566 -d dev './e2e_test/schema_registry/pb.slt'

echo "--- Kill cluster"
cargo make ci-kill

echo "--- e2e, ci-kafka-plus-pubsub, kafka and pubsub source"
RUST_LOG="info,risingwave_stream=info,risingwave_batch=info,risingwave_storage=info" \
cargo make ci-start ci-pubsub
./scripts/source/prepare_ci_kafka.sh
cargo run --bin prepare_ci_pubsub
sqllogictest -p 4566 -d dev './e2e_test/source/basic/*.slt'
sqllogictest -p 4566 -d dev './e2e_test/source/basic/old_row_format_syntax/*.slt'
sqllogictest -p 4566 -d dev './e2e_test/source/basic/alter/kafka.slt'

echo "--- e2e, kafka alter source"
chmod +x ./scripts/source/prepare_data_after_alter.sh
./scripts/source/prepare_data_after_alter.sh 2
sqllogictest -p 4566 -d dev './e2e_test/source/basic/alter/kafka_after_new_data.slt'

echo "--- e2e, kafka alter source again"
./scripts/source/prepare_data_after_alter.sh 3
sqllogictest -p 4566 -d dev './e2e_test/source/basic/alter/kafka_after_new_data_2.slt'

echo "--- Run CH-benCHmark"
./risedev slt -p 4566 -d dev './e2e_test/ch_benchmark/batch/ch_benchmark.slt'
./risedev slt -p 4566 -d dev './e2e_test/ch_benchmark/streaming/*.slt'
