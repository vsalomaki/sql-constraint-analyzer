-- Rows planted after co_populate.sql, each breaking one constraint but for the last two, whose order has no
-- customer; ../expected.json lists what they break.
INSERT INTO order_items (order_id,line_item_id,product_id,unit_price,quantity,shipment_id) VALUES (1,1,12,10,1,NULL);
INSERT INTO order_items (order_id,line_item_id,product_id,unit_price,quantity,shipment_id) VALUES (1,9,33,37,1,NULL);
INSERT INTO customers (customer_id,email_address,full_name) VALUES (393,'nameless@internalmail',NULL);
INSERT INTO orders (order_id,order_tms,customer_id,store_id,order_status) VALUES (1951,TO_TIMESTAMP('01-MAR-2022 10.00.00.000000000','DD-MON-YYYY HH24.MI.SS.FF'),9999,1,'OPEN');
INSERT INTO order_items (order_id,line_item_id,product_id,unit_price,quantity,shipment_id) VALUES (1951,1,1,10,1,NULL);
INSERT INTO order_items (order_id,line_item_id,product_id,unit_price,quantity,shipment_id) VALUES (1951,2,2,10,1,NULL);
COMMIT;
