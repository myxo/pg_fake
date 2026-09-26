CREATE TABLE squirrel_items (id integer PRIMARY KEY, label text, score integer);
INSERT INTO squirrel_items VALUES (1, 'alpha', 10), (2, 'beta', 20), (3, 'gamma', -5);
SELECT label FROM squirrel_items WHERE score > 0 ORDER BY label;
SELECT id FROM squirrel_items WHERE label = 'beta';
